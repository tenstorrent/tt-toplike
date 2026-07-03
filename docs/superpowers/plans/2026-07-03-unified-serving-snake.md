# Unified Serving Snake Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Make the `[i]` view one persistent snake with three behaviors — Roaming (cold), Growing (loading, built), Feeding (serving) — the last driven by live vLLM `/metrics`, replacing the curated Down-roster list.

**Architecture:** A new focused Prometheus parser turns a vLLM `/metrics` scrape into per-tick `ServingStats` (rates via counter deltas), folded onto `ServiceState.serving` by the existing background monitor. A new `Snake` coordinator owns a persistent identity (carried length/color/head) and delegates per behavior: Growing → the existing `LoadSnake`, Roaming → the existing `ModelStarfield` + a drifting body overlay, Feeding → a new calm signal-driven creature. The `[i]` render arm drives the single `Snake` from the existing state machine.

**Tech Stack:** Rust, ratatui, `ContainerProbe::http` (existing, curl-backed), `std::time`. No new dependencies.

## Global Constraints

- **No new dependencies.** Serving metrics come from the existing `ContainerProbe::http(port, "/metrics")`.
- **Zero panics.** Tolerant parse (bad/absent metrics → `None`), counter-reset clamps to 0, zero-dim render guards, char-safe truncation, saturating arithmetic.
- **Calm at rest.** The Feeding creature animates ONLY in proportion to real signals; idle (0 tok/s, 0 running) → gentle breathing only. No decorative motion.
- **Continuity by identity, not morphing.** One `Snake` persists and re-targets its behavior; do NOT attempt a tweened path between the three layouts.
- **Cross-platform.** `metrics.rs`, `snake.rs`, `ServingStats` are portable, NOT cfg-gated. Only the existing Linux-gated `inference_monitor.snapshot()` read stays gated.
- **SPDX header** on every new file.
- **CLAUDE.md gotcha:** adding a field to `ServiceState`/`TickSample` is a compile error at every struct literal, including test-only ones — update ALL enumerated sites.
- **Verify each task (real exit codes — NEVER pipe cargo/clippy through `tail`/`head`; append `; echo "exit: $?"`):**
  - `cargo test --locked --lib --features tui <filter> ; echo "exit: $?"`
  - `cargo clippy --locked --lib --bin tt-toplike-tui --features tui -- -D warnings ; echo "exit: $?"`
  - macOS cross-check: `cargo clippy --target aarch64-apple-darwin --no-default-features --features tui,json-backend -- -D warnings ; echo "exit: $?"` (skip w/ note if target absent).

## Current-state anchors
- `ServiceState` (`state.rs:82`) fields: `key,label,phase,cpu_pct,rss_bytes,rss_delta,kernel_count,kernel_delta,safetensors_fds,readiness,top_proc,last_log,progress,flat_ticks`.
- `TickSample` (`probe.rs:103`); built in `build_sample` (`monitor.rs:143`), folded by `fold_tick` (`monitor.rs:94`). `CADENCE_SECS = 5` (pub). `ContainerProbe::http(port: u16, path: &str) -> (u16, String)`.
- **ServiceState literal sites to update (add `serving: None`):** `inference_panel.rs` 123 (`down_placeholder`), 355 (`base`), 387, 406, 421, 436; `monitor.rs` 66 (`fresh_state`), 117 (`fold_tick` return), 336 (`ServiceState_zeroed` test helper); `inference_load.rs` 501 (`svc` test helper).
- **TickSample literal sites (add `metrics_text: String::new()` / real text):** `monitor.rs` 168 (`build_sample`), 344 (`sample_like`), 359, 376, 475.
- `LoadSnake` (`animation/inference_load.rs`): `new(w,h)`, `update(&mut self,&ServiceState,cadence_secs)`, `finish_if_ready(&mut self,&[ServiceState])->bool`, `render()->Vec<Line<'static>>`, `is_finishing()`. `featured_loading`/`trail_is_cold` in `inference_panel.rs`.
- `ModelStarfield` (`animation/model_starfield.rs`): `new(w,h)`, `update(&mut self,&[CatalogModel],chip_set)`, `render()->Vec<Line<'static>>`.
- `[i]` render arm (`ui/tui/mod.rs` ~673): currently `if show_snake {render_load_snake_view} else if trail_is_cold {render_model_starfield_view} else {render_inference_monitor_view}`; loop state `load_snake`/`model_starfield` ~407; tick-update arm ~535; `render_*_view` fns ~2978+. `CatalogRefresher` snapshot + arch already computed for the starfield.

## File Structure
- `src/workload/inference_server/metrics.rs` (new) — `VllmCounters`, `parse_vllm_metrics`, `ServingStats`, `ServingStats::fold` (Task 1).
- `src/workload/inference_server/{mod,state,probe,monitor}.rs` — `ServingStats` re-export, `ServiceState.serving`, `TickSample.metrics_text`, fetch+fold (Task 2).
- `src/animation/serving_creature.rs` (new) — the Feeding renderer (Task 3).
- `src/animation/snake.rs` (new) — the `Snake` coordinator (Task 4).
- `src/ui/tui/mod.rs` — drive the single `Snake`; remove the ready-list path (Task 5).

---

### Task 1: vLLM metrics parser + ServingStats fold

**Files:** Create `src/workload/inference_server/metrics.rs`; modify `src/workload/inference_server/mod.rs`.

**Interfaces:**
- Produces `VllmCounters { generation_tokens_total: u64, prompt_tokens_total: u64, requests_succeeded_total: u64, requests_errored_total: u64, requests_running: u32, requests_waiting: u32, kv_cache_usage: f32, ttft_sum: f64, ttft_count: u64 }` (derive `Debug, Clone, Copy, Default, PartialEq`); `parse_vllm_metrics(&str) -> Option<VllmCounters>`; `ServingStats { generation_tps: f32, prompt_tps: f32, completed_delta: u32, errored_delta: u32, requests_running: u32, requests_waiting: u32, kv_cache_usage: f32, ttft_avg_s: f32, counters: VllmCounters }` (derive `Debug, Clone, Copy, PartialEq`); `ServingStats::fold(prev: Option<&VllmCounters>, cur: &VllmCounters, cadence_secs: u32) -> ServingStats`.

- [ ] **Step 1: Write the failing tests** in `metrics.rs`:

```rust
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
        let prev = VllmCounters { generation_tokens_total: 826, prompt_tokens_total: 343, requests_succeeded_total: 4, requests_errored_total: 2, ..Default::default() };
        let cur = VllmCounters { generation_tokens_total: 826 + 4210, prompt_tokens_total: 343 + 100, requests_succeeded_total: 6, requests_errored_total: 2, requests_running: 1, requests_waiting: 2, kv_cache_usage: 0.04, ttft_sum: 1.10, ttft_count: 6 };
        let s = ServingStats::fold(Some(&prev), &cur, 5);
        assert!((s.generation_tps - 842.0).abs() < 0.5, "4210 gen tokens / 5s ≈ 842");
        assert_eq!(s.completed_delta, 2); // 6-4
        assert_eq!(s.errored_delta, 0);
        assert_eq!(s.requests_running, 1);
        assert_eq!(s.requests_waiting, 2);
        assert!((s.ttft_avg_s - (1.10 / 6.0) as f32).abs() < 1e-4);
    }

    #[test]
    fn fold_clamps_counter_reset_to_zero() {
        // Server restarted: cur < prev → no negative rates/deltas.
        let prev = VllmCounters { generation_tokens_total: 9000, requests_succeeded_total: 50, ..Default::default() };
        let cur = VllmCounters { generation_tokens_total: 10, requests_succeeded_total: 1, ..Default::default() };
        let s = ServingStats::fold(Some(&prev), &cur, 5);
        assert_eq!(s.generation_tps, 0.0);
        assert_eq!(s.completed_delta, 0);
    }

    #[test]
    fn fold_without_prev_is_zero_rates_but_keeps_gauges() {
        let cur = VllmCounters { requests_running: 3, kv_cache_usage: 0.5, ttft_sum: 2.0, ttft_count: 4, ..Default::default() };
        let s = ServingStats::fold(None, &cur, 5);
        assert_eq!(s.generation_tps, 0.0);
        assert_eq!(s.requests_running, 3);
        assert!((s.kv_cache_usage - 0.5).abs() < 1e-6);
        assert!((s.ttft_avg_s - 0.5).abs() < 1e-4); // 2.0/4
    }
}
```

- [ ] **Step 2: Run — fails.**

- [ ] **Step 3: Implement `metrics.rs`** (SPDX header first):

```rust
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
            if line.contains("finished_reason=\"error\"") || line.contains("finished_reason=\"abort\"") {
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
    pub fn fold(prev: Option<&VllmCounters>, cur: &VllmCounters, cadence_secs: u32) -> ServingStats {
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
```

- [ ] **Step 4: Wire `mod.rs`** — add `pub mod metrics;` and `pub use metrics::{parse_vllm_metrics, ServingStats, VllmCounters};` (NOT cfg-gated).

- [ ] **Step 5: Run tests + clippy + macOS cross-check — pass. Commit.**

```bash
git add src/workload/inference_server/metrics.rs src/workload/inference_server/mod.rs
git commit -m "feat(inference): vLLM /metrics parser + ServingStats (rates from counter deltas)"
```

---

### Task 2: Fold serving telemetry into the monitor

**Files:** Modify `src/workload/inference_server/state.rs`, `probe.rs`, `monitor.rs`.

**Interfaces:**
- Consumes `parse_vllm_metrics`, `ServingStats`, `VllmCounters`.
- Produces `ServiceState.serving: Option<ServingStats>`; `TickSample.metrics_text: String`.

- [ ] **Step 1: Add the fields.**
  - `state.rs`: add `pub serving: Option<ServingStats>,` to `ServiceState` (import `use crate::workload::inference_server::metrics::ServingStats;`).
  - `probe.rs`: add `pub metrics_text: String,` to `TickSample`.

- [ ] **Step 2: Update EVERY struct literal** (compile error otherwise — see anchors list). Add `serving: None,` to each `ServiceState { .. }` literal: `inference_panel.rs` (down_placeholder, base, and the 4 render-test literals), `monitor.rs` (fresh_state, fold_tick return, ServiceState_zeroed), `inference_load.rs` (svc). Add `metrics_text: String::new(),` to each `TickSample { .. }` literal in `monitor.rs` (build_sample, sample_like, and the 3 test literals). Run `cargo build --locked --lib --features tui ; echo "exit: $?"` until it compiles.

- [ ] **Step 3: Write the failing test** in `monitor.rs` tests (drives `fold_tick` twice to get a rate). Extend `FakeProbe` so `http` returns metrics text for the `/metrics` path:

```rust
    // In FakeProbe, make http() serve a metrics body for the /metrics path:
    //   fn http(&self, _port: u16, path: &str) -> (u16, String) {
    //       if path == "/metrics" {
    //           (200, "vllm:generation_tokens_total{model_name=\"M\"} 1000.0\n\
    //                  vllm:num_requests_running{model_name=\"M\"} 2.0\n".into())
    //       } else { (200, "{\"model_ready\":true}".into()) }
    //   }
    #[test]
    fn fold_tick_populates_serving_from_metrics() {
        let prev = fresh_state("k", "L");
        let s1 = fold_tick(&prev, sample_with_metrics("vllm:generation_tokens_total{m=\"M\"} 1000.0\nvllm:num_requests_running{m=\"M\"} 2.0\n"), &ModelProfile::default(), 5);
        assert!(s1.serving.is_some(), "serving populated when metrics present");
        assert_eq!(s1.serving.unwrap().requests_running, 2);
        // Next tick: +4210 gen tokens over 5s ≈ 842 tok/s (rate needs prev counters).
        let s2 = fold_tick(&s1, sample_with_metrics("vllm:generation_tokens_total{m=\"M\"} 5210.0\n"), &ModelProfile::default(), 5);
        assert!((s2.serving.unwrap().generation_tps - 842.0).abs() < 1.0);
    }
    #[test]
    fn fold_tick_serving_none_without_metrics() {
        let s = fold_tick(&fresh_state("k","L"), sample_with_metrics(""), &ModelProfile::default(), 5);
        assert!(s.serving.is_none());
    }
```
Add a `fn sample_with_metrics(text: &str) -> TickSample` test helper (clone `sample_like` and set `metrics_text: text.into()`).

- [ ] **Step 4: Implement.**
  - `build_sample`: after the health probe, add `let (_ms, metrics_text) = probe.http(port, "/metrics");` and set `metrics_text` on the returned `TickSample`. (Ignore the status — a non-200 just yields text that won't parse → `None`.)
  - `fold_tick`: after computing the other fields, add:
    ```rust
    let serving = crate::workload::inference_server::parse_vllm_metrics(&sample.metrics_text)
        .map(|cur| crate::workload::inference_server::ServingStats::fold(
            prev.serving.as_ref().map(|s| &s.counters),
            &cur,
            cadence_secs,
        ));
    ```
    and set `serving,` in the returned `ServiceState`.

- [ ] **Step 5: Run tests + clippy + macOS cross-check — pass. Commit.**

```bash
git add src/workload/inference_server/state.rs src/workload/inference_server/probe.rs src/workload/inference_server/monitor.rs src/ui/tui/inference_panel.rs src/animation/inference_load.rs
git commit -m "feat(inference): fetch /metrics per tick, fold ServingStats onto ServiceState"
```

---

### Task 3: Feeding creature renderer

**Files:** Create `src/animation/serving_creature.rs`; modify `src/animation/mod.rs`.

**Interfaces:**
- Consumes `crate::workload::inference_server::ServingStats`.
- Produces `ServingCreature::new()`, `update(&mut self, label: &str, uptime_secs: u64, serving: Option<&ServingStats>)`, `render(&self, width: usize, height: usize) -> Vec<ratatui::text::Line<'static>>`, and pure helpers `body_len(running: u32, max: usize) -> usize`, `is_active(s: &ServingStats) -> bool`.

- [ ] **Step 1: Write the failing tests** in `serving_creature.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::inference_server::{ServingStats, VllmCounters};

    fn stats(running: u32, tps: f32, completed: u32, errored: u32) -> ServingStats {
        ServingStats { generation_tps: tps, prompt_tps: 0.0, completed_delta: completed,
            errored_delta: errored, requests_running: running, requests_waiting: 0,
            kv_cache_usage: 0.0, ttft_avg_s: 0.0, counters: VllmCounters::default() }
    }

    #[test]
    fn body_length_tracks_in_flight_requests() {
        assert!(body_len(0, 40) < body_len(3, 40), "more in-flight → longer body");
        assert!(body_len(1000, 40) <= 40, "clamped to arena width");
    }

    #[test]
    fn idle_is_active_false_active_true() {
        assert!(!is_active(&stats(0, 0.0, 0, 0)), "0 tps + 0 running = idle");
        assert!(is_active(&stats(0, 120.0, 0, 0)), "tokens flowing = active");
        assert!(is_active(&stats(2, 0.0, 0, 0)), "requests in flight = active");
    }

    #[test]
    fn render_shows_stats_line_and_no_panic_on_zero_dims() {
        let mut c = ServingCreature::new();
        c.update("Qwen3-32B", 1454, Some(&stats(1, 842.0, 0, 0)));
        let text: String = c.render(70, 12).iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string())).collect();
        assert!(text.contains("Qwen3-32B"));
        assert!(text.contains("842") && text.contains("tok/s"));
        assert!(text.contains("1 running"));
        let _ = c.render(0, 0); // must not panic
    }

    #[test]
    fn no_metrics_renders_calm_alive_state() {
        let mut c = ServingCreature::new();
        c.update("Qwen3-32B", 60, None); // serving=None
        let text: String = c.render(70, 12).iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string())).collect();
        assert!(text.contains("Qwen3-32B"));
        assert!(text.to_lowercase().contains("serving") || text.to_lowercase().contains("ready"));
        assert!(!text.contains("tok/s"), "no rate shown without metrics");
    }
}
```

- [ ] **Step 2: Run — fails.**

- [ ] **Step 3: Implement `serving_creature.rs`** (SPDX header). Full code for the pure helpers; build the render against the `LoadSnake`/`ModelStarfield` canvas idiom (a `Vec<Vec<(char,Color)>>` guarded by `width>0 && height>0`, run-length grouped into `Line<'static>`):

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The Feeding behavior of the unified serving snake: a calm living creature
//! whose form encodes live vLLM serving signals directly. Near-still at rest;
//! animates only in proportion to real throughput/requests.

use crate::workload::inference_server::ServingStats;

/// Body length (cells) for `running` in-flight requests, clamped to `max`.
/// A small base so the creature is always visible; grows with concurrency.
pub fn body_len(running: u32, max: usize) -> usize {
    (4 + running as usize * 3).min(max.max(1))
}

/// Whether the server is doing work right now (drives motion vs. breathing).
pub fn is_active(s: &ServingStats) -> bool {
    s.generation_tps > 0.5 || s.requests_running > 0 || s.requests_waiting > 0
}
```

Render contract (mirror `LoadSnake::render`): guard zero dims → `vec![]`. Title line `{label}  serving · :{...} up {fmt_elapsed}` (reuse a local `fmt_elapsed(secs)` or import from `inference_load` if pub — if not pub, add a small local copy; keep it DRY by preferring a shared helper). Body: a horizontal creature of `body_len(running, width-2)` segments; **wave amplitude/animation driven by `generation_tps`** (scale a sine by a normalized tps; at 0 tps the body is flat = calm); **color-heat from `kv_cache_usage`** (cool→hot via `crate::animation::hsv_to_rgb`); a bright **pulse** glyph emitted when `completed_delta > 0` this update; a **tail fray** (frayed glyphs / `ERROR` tint) when `errored_delta > 0`; **dim queued pellets** to the right for `requests_waiting`. When `serving` is `None`, render a calm static body + "serving · ready" (no rates). Footer stats line (only when `serving` is `Some`): `{tps:.0} tok/s · {running} running · {waiting} queued · KV {kv*100:.0}% · TTFT {ttft:.2}s`. Build `Vec<Line<'static>>` from owned Strings. Track the pulse/fray as short frame-countdowns set in `update`.

- [ ] **Step 4: Wire `src/animation/mod.rs`** — `pub mod serving_creature;` + `pub use serving_creature::ServingCreature;`.

- [ ] **Step 5: Run tests + clippy + macOS cross-check — pass. Commit.**

```bash
git add src/animation/serving_creature.rs src/animation/mod.rs
git commit -m "feat(animation): ServingCreature — calm signal-driven serving snake"
```

---

### Task 4: The Snake coordinator

**Files:** Create `src/animation/snake.rs`; modify `src/animation/mod.rs`.

**Interfaces:**
- Consumes `LoadSnake`, `ModelStarfield`, `ServingCreature`, `CatalogModel`, `ServiceState`, `ServingStats`, `featured_loading`/`trail_is_cold`.
- Produces `Snake::new()`, `SnakeWorld` (the per-frame input), `Snake::update(&mut self, world: &SnakeWorld)`, `Snake::render(&self, width: usize, height: usize) -> Vec<Line<'static>>`, `Snake::behavior(&self) -> Behavior`, `Behavior { Roaming, Growing, Feeding }`; a pure `choose_behavior(rows: &[ServiceState]) -> Behavior`.

The coordinator is thin: it owns the three sub-renderers + a persistent carried identity (a `body_len: usize` and `frame: u64` shared across behaviors so the creature reads as continuous), picks a behavior each frame from the snapshot, and delegates rendering. Continuity = identity + carried length, NOT morphing (per the spec).

- [ ] **Step 1: Write the failing test** in `snake.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::inference_server::{Phase, Readiness, ServiceState};
    fn svc(phase: Phase) -> ServiceState { /* same shape as inference_load::tests::svc, serving: None */ ... }

    #[test]
    fn behavior_follows_the_state_machine() {
        let mut loading = svc(Phase::Loading);
        let mut ready = svc(Phase::Ready);
        let mut down = svc(Phase::Down);
        assert_eq!(choose_behavior(&[]), Behavior::Roaming);
        assert_eq!(choose_behavior(std::slice::from_ref(&down)), Behavior::Roaming);
        assert_eq!(choose_behavior(std::slice::from_ref(&loading)), Behavior::Growing);
        assert_eq!(choose_behavior(std::slice::from_ref(&ready)), Behavior::Feeding);
        // loading wins over a co-present ready
        assert_eq!(choose_behavior(&[ready, loading]), Behavior::Growing);
    }
}
```
`choose_behavior`: `if featured_loading(rows).is_some() { Growing } else if rows.iter().any(|s| s.phase == Phase::Ready) { Feeding } else { Roaming }`.

- [ ] **Step 2: Run — fails.**

- [ ] **Step 3: Implement `snake.rs`** (SPDX header). `SnakeWorld` carries everything a frame needs (borrowed): `rows: &[ServiceState]`, `catalog: &[CatalogModel]`, `arch: &str`, `cadence_secs: u32`. `Snake` owns a `ModelStarfield`, a `LoadSnake`, a `ServingCreature`, and the render dims are passed to `render`. `update` calls `choose_behavior`, then updates the active sub-renderer: Roaming → `starfield.update(catalog, arch)`; Growing → `load_snake.update(featured_service, cadence)` (+ `finish_if_ready` handled by caller as today, or fold it in); Feeding → pick the Ready service, compute uptime, `serving_creature.update(label, uptime, service.serving.as_ref())`. `render` delegates to the active sub-renderer's render (Roaming overlays a small drifting body on `starfield.render()` — for v1 the overlay may be a single moving head glyph row appended/merged; keep it simple and non-panicking). `behavior()` returns the current `Behavior`.
   NOTE on the loading→ready burst: preserve the existing gold-burst by letting the coordinator keep `Growing` (rendering `load_snake`) while `load_snake.is_finishing()` even after the service goes Ready — mirror the current `mod.rs` `show_snake` short-circuit logic inside `update`/`behavior` so the burst still plays before Feeding takes over.

- [ ] **Step 4: Wire `src/animation/mod.rs`** — `pub mod snake;` + `pub use snake::{Snake, SnakeWorld, Behavior};`.

- [ ] **Step 5: Run tests + clippy + macOS cross-check — pass. Commit.**

```bash
git add src/animation/snake.rs src/animation/mod.rs
git commit -m "feat(animation): Snake coordinator — one creature across roaming/growing/feeding"
```

---

### Task 5: Drive the single Snake from the `[i]` view

**Files:** Modify `src/ui/tui/mod.rs`.

**Interfaces:** Consumes `Snake`, `SnakeWorld`. Produces `render_snake_view(f: &mut Frame, snake: &Snake)`.

- [ ] **Step 1: Replace the loop state** (~lines 407-408) — remove the separate `model_starfield`/`model_starfield_dims`/`load_snake`/`load_snake_dims` locals and add:
```rust
    let mut snake = crate::animation::Snake::new();
```
(The sub-renderers now live inside `Snake`; sizing is passed at render time, so no dims tracking here.)

- [ ] **Step 2: Replace the tick-update arm** (the `should_tick` `DisplayMode::InferenceMonitor =>` block, ~535) — build a `SnakeWorld` and call `snake.update(&world)`. On Linux the rows come from `inference_monitor.snapshot()`; the catalog from `catalog_refresher.snapshot()`; arch from `backend.devices().first().map(|d| d.architecture.name()).unwrap_or("Unknown")`; cadence = `CADENCE_SECS`. Off Linux, rows = `&[]`.

- [ ] **Step 3: Replace the render arm** (draw-closure `DisplayMode::InferenceMonitor =>`, ~678) — with a single `render_snake_view(f, &snake);`. Remove the `featured_loading`/`trail_is_cold`/`service_rows` branching and the now-unused `render_model_starfield_view` / `render_load_snake_view` / `render_inference_monitor_view` calls from this arm. Delete `service_cursor`/up-down handling for this view if it's now dead (the roster list is gone) — verify no other code references it; if the Up/Down handlers (~1023) are InferenceMonitor-only, remove them.

- [ ] **Step 4: Add `render_snake_view`** near the old view fns:
```rust
/// Full-screen render of the unified serving snake (see `crate::animation::Snake`).
fn render_snake_view(f: &mut Frame, snake: &crate::animation::Snake) {
    let area = f.area();
    f.render_widget(Block::default().style(Style::default().bg(colors::rgb(0, 0, 0))), area);
    f.render_widget(Paragraph::new(snake.render(area.width as usize, area.height as usize)), area);
}
```
Remove `render_model_starfield_view` and `render_load_snake_view` if they have no remaining callers (grep first); keep `render_inference_monitor_view` only if still referenced elsewhere (e.g. an explain/legend path) — otherwise remove it and `service_rows`/`down_placeholder` if now dead (grep; if `service_rows` tests remain the only callers, remove them too, or leave the fn if it's still exercised — prefer removing genuinely dead code but do not break other callers).

- [ ] **Step 5: Verify + manual check + commit.** Full lib suite, `RUSTFLAGS="-D warnings"` build, clippy, macOS cross-check (real exit codes). Manual: `tt-toplike-tui`, press `i` — with the Qwen3-32B server up you should see the Feeding creature + live stats line (tok/s etc.); with nothing running, Roaming over the starfield; during a load, Growing. Then commit.

```bash
git add src/ui/tui/mod.rs
git commit -m "feat(tui): [i] view is one unified snake (roaming/growing/feeding)"
```

---

## Self-Review

**Spec coverage:** persistent one-snake identity → Task 4 `Snake` (carried length/frame) + Task 5 single render; state machine (loading>ready>cold) → Task 4 `choose_behavior` + burst preservation; Roaming over starfield → Task 4 delegation; Growing (built) → Task 4 delegates to `LoadSnake`; Feeding signal→motion (length∝running, waves∝tps, heat∝kv, pulse on completed, fray on error, queued pellets, calm idle) → Task 3 `ServingCreature`; serving telemetry (vLLM /metrics parse, rates via deltas, counter-reset clamp, ttft avg) → Task 1; acquisition via existing `ContainerProbe::http` + fold onto `ServiceState.serving` → Task 2; error handling (no/bad metrics → None → calm alive; non-vLLM → None; counter reset → 0; zero dims/multibyte guarded) → Tasks 1/2/3; remove the Down-roster list → Task 5; testing → each task's pure tests + render smoke. ✓

**Placeholder scan:** `choose_behavior`, `body_len`, `is_active`, `parse_vllm_metrics`, `ServingStats::fold` are fully coded + tested. Task 3 `render`, Task 4 coordinator `render`/`update`, and Task 5 wiring are described against exact anchors + the existing `LoadSnake`/`ModelStarfield` canvas and `render_*_view` patterns rather than pasted verbatim (acceptable per "follow existing patterns"). The Task 4 test `svc(..)` helper is marked to mirror `inference_load::tests::svc` with `serving: None`. No TBD/"handle edge cases".

**Type consistency:** `VllmCounters`/`parse_vllm_metrics`/`ServingStats{fold}` (Task 1) ↔ `ServiceState.serving`/`TickSample.metrics_text` (Task 2) ↔ `ServingCreature{new,update,render}`/`body_len`/`is_active` (Task 3) ↔ `Snake{new,update,render,behavior}`/`SnakeWorld`/`Behavior`/`choose_behavior` (Task 4) ↔ `render_snake_view` (Task 5). `ServingStats::fold(Option<&VllmCounters>, &VllmCounters, u32)` signature consistent between Task 1 def and Task 2 caller. `CADENCE_SECS` reused in Task 2/5. Continuity-by-identity (not morphing) stated in spec + Task 4.

**Risk note for the executor:** Task 5 deletes dead render paths — grep every removed fn/local (`render_model_starfield_view`, `render_load_snake_view`, `render_inference_monitor_view`, `service_rows`, `down_placeholder`, `service_cursor`) for remaining callers before removing; if a fn is still used by a legend/explain/test path, leave it. Prefer leaving a fn over breaking a caller.
