# Serving Dashboard + ANSI-Shaded Loading Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Turn the Feeding view into an adaptive serving dashboard (throughput timeline, token-exhaust snake, request swimlanes, TT silicon strip) driven by real vLLM `/metrics` + device telemetry, and reskin the loading (Growing) render with classic ANSI-art shading.

**Architecture:** Extend the metrics parser with per-stage latency + prefix/preemption stats. Add pure panel helpers (sparkline, swimlane segments, chip cell, exhaust particles) and a `ChipReading` snapshot threaded through `SnakeWorld`. The `ServingCreature` grows into an adaptive compositor that lays out the panels on one canvas. The loading render reuses existing `common.rs` shading helpers for block-ramp + hue-sweep gradients.

**Tech Stack:** Rust, ratatui. Reuses `crate::animation::common::{value_to_block_char, hsv_to_rgb, lerp, temp_to_hue, ease_in_out, BLOCK_CHARS}`. No new dependencies.

## Global Constraints

- **No new dependencies.** Serving metrics via the existing `ContainerProbe::http`; shading via existing `common.rs` helpers.
- **Every motion maps to a real signal; calm at rest.** Idle (0 tps / 0 running) → no exhaust, flat snake, static panels.
- **No panics.** Tolerant parse (absent metric → 0), zero-dim guard (`vec![]`), clamped canvas indices, char-safe (`.chars()`) truncation, saturating casts, bounded history ring.
- **Adaptive degradation.** Regions omitted below a minimum size in this collapse order: timeline → swimlanes → chip strip → (snake + stats always survive); width < 44 → snake-only.
- **Aggregate-only swimlanes.** vLLM `/metrics` has no per-request IDs — lanes are `requests_running` lanes split by *average* per-stage times, not per-request traces. Label/behave honestly.
- **Box-level silicon.** The chip strip shows all TT devices on the box (server→chip mapping isn't exposed).
- **Cross-platform.** All new code portable, NOT cfg-gated; only the existing `inference_monitor.snapshot()` read stays Linux-gated.
- **SPDX header** on any new file.
- **Verify each task (real exit codes — NEVER pipe cargo/clippy through `tail`/`head`; append `; echo "exit: $?"`):**
  - `cargo test --locked --lib --features tui <filter> ; echo "exit: $?"`
  - `cargo clippy --locked --lib --bin tt-toplike-tui --features tui -- -D warnings ; echo "exit: $?"`
  - macOS cross-check: `cargo clippy --target aarch64-apple-darwin --no-default-features --features tui,json-backend -- -D warnings ; echo "exit: $?"` (skip w/ note if target absent).

## Current-state anchors
- `metrics.rs`: `VllmCounters` (Default) fields today: `generation_tokens_total, prompt_tokens_total, requests_succeeded_total, requests_errored_total, requests_running, requests_waiting, kv_cache_usage, ttft_sum, ttft_count`. `ServingStats` (no Default) fields: `generation_tps, prompt_tps, completed_delta, errored_delta, requests_running, requests_waiting, kv_cache_usage, ttft_avg_s, counters`. `parse_vllm_metrics` uses `is_metric(line, name)` (exact name match before `{`) + `line_value`. `ServingStats::fold(prev: Option<&VllmCounters>, cur, cadence)`.
- `serving_creature.rs`: `ServingCreature { frame, label, uptime_secs, cpu_pct, rss_bytes, serving: Option<ServingStats>, pulse_left, fray_left }`; `update(&mut self, svc: &ServiceState, uptime_secs: u64)`; `render(&self, width, height) -> Vec<Line<'static>>` (title + two-column snake/panel via a `Vec<Vec<(char,Color)>>` canvas + `row_to_line`); pure `body_len`, `is_active`, `panel_rows`. Tests use a `stats(running,tps,completed,errored)` helper (a **ServingStats literal** — must gain any new field) and `ready_svc(label,cpu,rss,serving)`.
- `snake.rs`: `SnakeWorld { rows, catalog, arch, cadence_secs, width, height }`; `Snake::update` Feeding arm calls `self.serving_creature.update(svc, uptime)`.
- `inference_load.rs`: `LoadSnake::render` draws boxes/body via a canvas; `draw_chamber` helper; colors via `colors::{SUCCESS,WARNING,ERROR}`. `fmt_bytes`/`fmt_elapsed`/`group_thousands` pub.
- `common.rs`: `value_to_block_char(f32)->char` (`·░▒▓█`), `value_to_char_intensity(v,&[char])`, `BLOCK_CHARS`, `hsv_to_rgb(h,s,v)->Color`, `lerp(a,b,t)`, `temp_to_hue(c)->f32`, `ease_in_out(t)`. All re-exported via `crate::animation::*`.
- `[i]` tick wiring (`mod.rs` ~549): builds `SnakeWorld` from `inference_monitor.snapshot()` (Linux) / catalog / arch / size; `backend.devices()` + `backend.telemetry(idx)` give `Telemetry { power, asic_temperature, aiclk, .. }` and `Device { index, architecture }` (`architecture.name()` → "Blackhole"/…).

## File Structure
- `src/workload/inference_server/metrics.rs` — Task 1 parser/stats extensions.
- `src/animation/inference_load.rs` — Task 2 loading shading.
- `src/animation/serving_panels.rs` (new) — Task 3 pure panels + `ChipReading`.
- `src/animation/snake.rs`, `src/ui/tui/mod.rs` — Task 4 `SnakeWorld.chips` + wiring.
- `src/animation/serving_creature.rs` — Task 4 update plumbing + Task 5 compositor render.

---

### Task 1: Metrics — per-stage latency, prefix cache, preemptions

**Files:** Modify `src/workload/inference_server/metrics.rs`; update the `stats(..)` ServingStats literal in `src/animation/serving_creature.rs` tests.

**Interfaces:**
- Consumes existing `VllmCounters`/`ServingStats`/`parse_vllm_metrics`/`fold`.
- Produces new `VllmCounters` fields `queue_time_sum: f64, queue_time_count: u64, prefill_time_sum: f64, prefill_time_count: u64, decode_time_sum: f64, decode_time_count: u64, tpot_sum: f64, tpot_count: u64, prefix_queries_total: u64, prefix_hits_total: u64, preemptions_total: u64`; new `ServingStats` fields `queue_avg_s: f32, prefill_avg_s: f32, decode_avg_s: f32, tpot_avg_s: f32, prefix_hit_rate: f32, preemptions_delta: u32`.

- [ ] **Step 1: Failing test** appended to `metrics.rs` tests:

```rust
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
            queue_time_sum: 2.0, queue_time_count: 4,
            prefill_time_sum: 4.0, prefill_time_count: 4,
            decode_time_sum: 12.0, decode_time_count: 4,
            tpot_sum: 0.8, tpot_count: 40,
            prefix_queries_total: 200, prefix_hits_total: 150,
            preemptions_total: 5,
            ..Default::default()
        };
        let prev = VllmCounters { preemptions_total: 3, ..Default::default() };
        let s = ServingStats::fold(Some(&prev), &cur, 5);
        assert!((s.queue_avg_s - 0.5).abs() < 1e-4);   // 2/4
        assert!((s.prefill_avg_s - 1.0).abs() < 1e-4); // 4/4
        assert!((s.decode_avg_s - 3.0).abs() < 1e-4);  // 12/4
        assert!((s.tpot_avg_s - 0.02).abs() < 1e-4);   // 0.8/40
        assert!((s.prefix_hit_rate - 0.75).abs() < 1e-4); // 150/200
        assert_eq!(s.preemptions_delta, 2);            // 5-3
    }
```

- [ ] **Step 2: Run — fails.**

- [ ] **Step 3: Implement.** Add the new fields to `VllmCounters` (Default derive already covers them). In `parse_vllm_metrics`, add `else if` arms (same `is_metric` exact-name style) for each new metric — `_sum` parsed to the `f64` field, `_count`/counters via `v.max(0.0) as u64`. In `ServingStats`, add the six new fields; in `fold`, compute:

```rust
        let avg = |sum: f64, count: u64| -> f32 {
            if count > 0 { (sum / count as f64) as f32 } else { 0.0 }
        };
        // ... inside fold, after the existing rate/ttft computation:
        let prefix_hit_rate = if cur.prefix_queries_total > 0 {
            (cur.prefix_hits_total as f64 / cur.prefix_queries_total as f64) as f32
        } else {
            0.0
        };
        let preemptions_delta = match prev {
            Some(p) => cur.preemptions_total.saturating_sub(p.preemptions_total) as u32,
            None => 0,
        };
        // set in the returned struct:
        //   queue_avg_s: avg(cur.queue_time_sum, cur.queue_time_count),
        //   prefill_avg_s: avg(cur.prefill_time_sum, cur.prefill_time_count),
        //   decode_avg_s: avg(cur.decode_time_sum, cur.decode_time_count),
        //   tpot_avg_s: avg(cur.tpot_sum, cur.tpot_count),
        //   prefix_hit_rate, preemptions_delta,
```

- [ ] **Step 4: Fix the ServingStats literal** in `serving_creature.rs` tests — the `stats(..)` helper constructs a `ServingStats { .. }` literal; add `queue_avg_s: 0.0, prefill_avg_s: 0.0, decode_avg_s: 0.0, tpot_avg_s: 0.0, prefix_hit_rate: 0.0, preemptions_delta: 0,` so it compiles.

- [ ] **Step 5: Run tests + clippy + macOS cross-check — pass. Commit.**

```bash
git add src/workload/inference_server/metrics.rs src/animation/serving_creature.rs
git commit -m "feat(inference): parse per-stage latency + prefix cache + preemptions into ServingStats"
```

---

### Task 2: ANSI-shaded loading render

**Files:** Modify `src/animation/inference_load.rs`.

**Interfaces:** Consumes `crate::animation::{value_to_block_char, hsv_to_rgb, lerp}`. Produces a pure `journey_hue(t: f32) -> f32` (0→teal ~170°, 0.5→amber ~40°, 1→gold ~48°... see below) used by the render; render behavior otherwise unchanged.

- [ ] **Step 1: Failing test** in `inference_load.rs` tests:

```rust
    #[test]
    fn journey_hue_sweeps_compile_to_ready() {
        // Cool (compile) at the start, warm (load/ready) at the end; monotone-ish.
        let cold = journey_hue(0.0);
        let hot = journey_hue(1.0);
        assert!(cold > hot, "hue moves from teal (~170) down toward gold (~45)");
        assert!((journey_hue(0.5) - journey_hue(0.5)).abs() < 1e-9); // deterministic
        assert!(journey_hue(-1.0) == journey_hue(0.0)); // clamped
        assert!(journey_hue(2.0) == journey_hue(1.0));
    }
```

- [ ] **Step 2: Run — fails.**

- [ ] **Step 3: Implement `journey_hue`** and rework the render. Full code for the helper:

```rust
/// Hue (degrees) along the cold→ready journey: teal (compile) → amber → gold.
/// `t` is clamped to [0,1]. Feed to `hsv_to_rgb(journey_hue(t), s, v)`.
pub fn journey_hue(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    // 170° teal → 45° gold, linear.
    crate::animation::lerp(170.0, 45.0, t)
}
```

  Then in `LoadSnake::render` (and its `draw_chamber` helper), replace the flat body glyph + single phase color with shaded fills:
  - **Block ramp:** instead of a constant `●`, pick the body glyph from local intensity via `value_to_block_char(intensity)` where `intensity` = per-cell fill closeness + a shimmer `0.15 * (frame*0.2 + x).sin()` clamped to [0,1]. Filled interior → high intensity (`█▓`), the frontier → mid (`▒░`), giving a dithered gradient edge.
  - **Hue sweep:** color each box/cell via `hsv_to_rgb(journey_hue(global_t), 0.8, brightness)` where `global_t` is the cell's position along the whole compile→load→ready track (0 at the start of compile, 1 at ready), so the palette flows teal→amber→gold rather than three flat colors. `brightness` rises toward the frontier for the bright leading edge.
  - **Alarm** still overrides to `colors::ERROR` (unchanged). Determinate/momentum fill math and box-open logic **unchanged** — this is purely how filled cells are colored/glyphed.
  Keep it panic-safe (existing bounds) and char-safe. Add one smoke test that a Loading render at a normal size contains at least one `▓` or `▒` (shaded block) rather than only `●`.

- [ ] **Step 4: Run tests + clippy + macOS cross-check — pass. Commit.**

```bash
git add src/animation/inference_load.rs
git commit -m "feat(animation): ANSI-art shading (block ramp + hue sweep) for the loading snake"
```

---

### Task 3: Pure serving panels + `ChipReading`

**Files:** Create `src/animation/serving_panels.rs`; modify `src/animation/mod.rs`.

**Interfaces:**
- Consumes `crate::animation::{value_to_char_intensity, BLOCK_CHARS}`.
- Produces: `ChipReading { index: usize, arch: &'static str, power_w: Option<f32>, temp_c: Option<f32>, aiclk_mhz: Option<u32> }` (derive Debug, Clone); `sparkline(samples: &[f32], width: usize) -> String`; `lane_segments(running: u32, queue_avg: f32, prefill_avg: f32, decode_avg: f32, width: usize, max_lanes: usize) -> Vec<[usize; 3]>` (per lane: `[queue_w, prefill_w, decode_w]` summing ≤ width); `format_chip(c: &ChipReading) -> String`; `exhaust_count(generation_tps: f32) -> usize` (particles to emit this frame).

- [ ] **Step 1: Failing tests** in `serving_panels.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sparkline_maps_range_to_blocks_and_exact_width() {
        let s = sparkline(&[0.0, 0.5, 1.0], 3);
        assert_eq!(s.chars().count(), 3);
        let ch: Vec<char> = s.chars().collect();
        assert_eq!(ch[0], '▁'); // low
        assert_eq!(ch[2], '█'); // high
        assert_eq!(sparkline(&[], 5).chars().count(), 5); // empty → blanks, exact width
        assert_eq!(sparkline(&[1.0], 0).chars().count(), 0); // zero width safe
    }
    #[test]
    fn lane_segments_split_by_stage_ratio() {
        // queue:prefill:decode = 1:1:2 over width 8 (minus dividers is impl detail);
        // decode should be the widest segment; 0 running → no lanes.
        let lanes = lane_segments(2, 1.0, 1.0, 2.0, 20, 4);
        assert_eq!(lanes.len(), 2, "one segment-set per running request, capped");
        let [q, p, d] = lanes[0];
        assert!(d >= p && d >= q, "decode widest (2:1:1 ratio)");
        assert!(q + p + d <= 20);
        assert!(lane_segments(0, 1.0, 1.0, 1.0, 20, 4).is_empty());
        // missing stage times (all 0) → single active bar (one non-zero segment)
        let fallback = lane_segments(1, 0.0, 0.0, 0.0, 20, 4);
        assert_eq!(fallback.len(), 1);
        assert!(fallback[0].iter().sum::<usize>() > 0);
    }
    #[test]
    fn format_chip_shows_fields_and_placeholders() {
        let c = ChipReading { index: 0, arch: "Blackhole", power_w: Some(92.0), temp_c: Some(78.0), aiclk_mhz: Some(1350) };
        let s = format_chip(&c);
        assert!(s.contains("78") && s.contains("92") && (s.contains("1.35") || s.contains("1350")));
        let none = ChipReading { index: 1, arch: "Blackhole", power_w: None, temp_c: None, aiclk_mhz: None };
        assert!(format_chip(&none).contains('—'));
    }
    #[test]
    fn exhaust_count_scales_with_tps_and_calm_at_zero() {
        assert_eq!(exhaust_count(0.0), 0);
        assert!(exhaust_count(1000.0) > exhaust_count(100.0));
    }
}
```

- [ ] **Step 2: Run — fails.**

- [ ] **Step 3: Implement `serving_panels.rs`** (SPDX header). Full code:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Pure layout helpers for the serving dashboard's panels — sparkline, request
//! swimlane segments, chip-strip formatting, token-exhaust emission. Kept free
//! of rendering state so each is unit-tested directly; the compositor in
//! `serving_creature.rs` places their output on the canvas.

use crate::animation::inference_load::{fmt_bytes as _fmt_bytes}; // (kept if needed)

/// One TT device's live readings for the silicon strip (box-level).
#[derive(Debug, Clone)]
pub struct ChipReading {
    pub index: usize,
    pub arch: &'static str,
    pub power_w: Option<f32>,
    pub temp_c: Option<f32>,
    pub aiclk_mhz: Option<u32>,
}

const SPARK: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Render the last `width` samples as a sparkline, normalized to the sample max.
/// Empty samples → blanks; exact `width` chars; zero width → empty.
pub fn sparkline(samples: &[f32], width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if samples.is_empty() {
        return " ".repeat(width);
    }
    let max = samples.iter().cloned().fold(0.0_f32, f32::max).max(1e-6);
    let start = samples.len().saturating_sub(width);
    let recent = &samples[start..];
    let mut out = String::with_capacity(width);
    // Left-pad with blanks so the line is exactly `width` and right-aligned.
    for _ in 0..width.saturating_sub(recent.len()) {
        out.push(' ');
    }
    for &v in recent {
        let idx = ((v / max).clamp(0.0, 1.0) * (SPARK.len() - 1) as f32) as usize;
        out.push(SPARK[idx.min(SPARK.len() - 1)]);
    }
    out
}

/// Per-lane `[queue_w, prefill_w, decode_w]` segment widths for `running` lanes
/// (capped at `max_lanes`), split by the stage-time ratio across `width`. When
/// all stage times are 0 (metrics absent), returns one "active" bar per lane
/// (all width in the decode segment).
pub fn lane_segments(
    running: u32,
    queue_avg: f32,
    prefill_avg: f32,
    decode_avg: f32,
    width: usize,
    max_lanes: usize,
) -> Vec<[usize; 3]> {
    let lanes = (running as usize).min(max_lanes);
    if lanes == 0 || width == 0 {
        return Vec::new();
    }
    let total = queue_avg + prefill_avg + decode_avg;
    let seg = if total <= 0.0 {
        [0, 0, width] // fallback: single active (decode) bar
    } else {
        let q = ((queue_avg / total) * width as f32).round() as usize;
        let p = ((prefill_avg / total) * width as f32).round() as usize;
        let d = width.saturating_sub(q + p);
        [q.min(width), p.min(width.saturating_sub(q)), d]
    };
    vec![seg; lanes]
}

/// Format a chip strip cell: `BH0 78°C 92W 1.35GHz` (`—` for absent fields).
pub fn format_chip(c: &ChipReading) -> String {
    let abbr = match c.arch {
        "Blackhole" => "BH",
        "Wormhole" => "WH",
        "Grayskull" => "GS",
        _ => "TT",
    };
    let temp = c.temp_c.map(|t| format!("{t:.0}°C")).unwrap_or_else(|| "—".into());
    let pow = c.power_w.map(|p| format!("{p:.0}W")).unwrap_or_else(|| "—".into());
    let clk = c
        .aiclk_mhz
        .map(|m| format!("{:.2}GHz", m as f32 / 1000.0))
        .unwrap_or_else(|| "—".into());
    format!("{abbr}{}  {temp}  {pow}  {clk}", c.index)
}

/// Particles to emit this frame from the token exhaust, scaled by decode tps.
/// 0 tps → 0 (calm at rest). Saturates so a burst stays bounded.
pub fn exhaust_count(generation_tps: f32) -> usize {
    if generation_tps <= 0.5 {
        0
    } else {
        ((generation_tps / 120.0).clamp(0.0, 4.0) as usize) + 1
    }
}
```
(Drop the unused `_fmt_bytes` import if clippy flags it — it's a placeholder; remove it.)

- [ ] **Step 4: Wire `src/animation/mod.rs`** — `pub mod serving_panels;` + `pub use serving_panels::{ChipReading, sparkline, lane_segments, format_chip, exhaust_count};`.

- [ ] **Step 5: Run tests + clippy + macOS cross-check — pass. Commit.**

```bash
git add src/animation/serving_panels.rs src/animation/mod.rs
git commit -m "feat(animation): pure serving-dashboard panels (sparkline, swimlanes, chip cell, exhaust)"
```

---

### Task 4: Chip telemetry + history plumbing

**Files:** Modify `src/animation/snake.rs`, `src/ui/tui/mod.rs`, `src/animation/serving_creature.rs`.

**Interfaces:**
- Consumes `ChipReading`.
- Produces `SnakeWorld.chips: &'a [ChipReading]`; extends `ServingCreature::update(&mut self, svc: &ServiceState, uptime_secs: u64, chips: &[ChipReading])` and adds a bounded throughput history ring the compositor (Task 5) reads.

- [ ] **Step 1: Add `SnakeWorld.chips`** in `snake.rs`: `pub chips: &'a [ChipReading],` (import `use crate::animation::ChipReading;`). Update the test `world(..)` helper to add `chips: &[]`.

- [ ] **Step 2: Failing test** in `serving_creature.rs` tests — update stores chips + pushes history:

```rust
    #[test]
    fn update_records_chips_and_throughput_history() {
        let mut c = ServingCreature::new();
        let chips = [ChipReading { index: 0, arch: "Blackhole", power_w: Some(90.0), temp_c: Some(70.0), aiclk_mhz: Some(1350) }];
        let svc = ready_svc("M", 2.0, 1024, Some(stats(1, 842.0, 0, 0)));
        c.update(&svc, 10, &chips);
        c.update(&svc, 11, &chips);
        assert_eq!(c.chips().len(), 1);
        assert!(c.tps_history().len() >= 2, "history grows each update");
        // Ring is bounded.
        for _ in 0..500 { c.update(&svc, 12, &chips); }
        assert!(c.tps_history().len() <= HISTORY_CAP);
    }
```
(Add `use crate::animation::ChipReading;` to the test module.)

- [ ] **Step 3: Implement.** In `serving_creature.rs`: add fields `chips: Vec<ChipReading>` and `tps_history: Vec<f32>`, a `const HISTORY_CAP: usize = 64;`, and accessors `pub fn chips(&self) -> &[ChipReading]` / `pub fn tps_history(&self) -> &[f32]`. Change `update` to the new signature; inside, `self.chips = chips.to_vec();` and push `self.serving.map(|s| s.generation_tps).unwrap_or(0.0)` to `tps_history`, truncating from the front when it exceeds `HISTORY_CAP` (`if self.tps_history.len() > HISTORY_CAP { self.tps_history.remove(0); }`). Import `ChipReading`.

- [ ] **Step 4: Wire the coordinator + `[i]`.** In `snake.rs` Feeding arm: `self.serving_creature.update(svc, uptime, world.chips);`. In `mod.rs` tick wiring, build the chips slice before constructing `SnakeWorld` and pass it:

```rust
                    let chips: Vec<crate::animation::ChipReading> = backend
                        .devices()
                        .iter()
                        .map(|d| {
                            let t = backend.telemetry(d.index);
                            crate::animation::ChipReading {
                                index: d.index,
                                arch: d.architecture.name(),
                                power_w: t.and_then(|t| t.power),
                                temp_c: t.and_then(|t| t.asic_temperature),
                                aiclk_mhz: t.and_then(|t| t.aiclk),
                            }
                        })
                        .collect();
```
   then add `chips: &chips,` to the `SnakeWorld { .. }`.

- [ ] **Step 5: Run tests + clippy + macOS cross-check — pass. Commit.**

```bash
git add src/animation/snake.rs src/ui/tui/mod.rs src/animation/serving_creature.rs
git commit -m "feat(animation): thread TT chip telemetry + throughput history into the serving snake"
```

---

### Task 5: Serving dashboard compositor

**Files:** Modify `src/animation/serving_creature.rs`.

**Interfaces:** Consumes Task 3 panels (`sparkline`, `lane_segments`, `format_chip`, `exhaust_count`, `ChipReading`) + Task 4 state (`chips`, `tps_history`). Reworks `ServingCreature::render` into the adaptive dashboard.

- [ ] **Step 1: Failing test** in `serving_creature.rs` tests:

```rust
    #[test]
    fn dashboard_composes_regions_at_large_size_and_degrades() {
        let mut c = ServingCreature::new();
        let chips = [ChipReading { index: 0, arch: "Blackhole", power_w: Some(92.0), temp_c: Some(78.0), aiclk_mhz: Some(1350) }];
        let mut svc = ready_svc("Qwen3-32B", 2.8, 70 * 1024 * 1024 * 1024, Some(stats(2, 842.0, 0, 0)));
        // give swimlanes some stage times
        if let Some(s) = svc.serving.as_mut() { s.queue_avg_s = 0.5; s.prefill_avg_s = 1.0; s.decode_avg_s = 3.0; }
        c.update(&svc, 1454, &chips);
        let big: String = c.render(100, 30).iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string())).collect();
        assert!(big.contains("Qwen3-32B"));              // title
        assert!(big.contains("tok/s"));                  // timeline / stats
        assert!(big.contains("requests"));               // swimlane header
        assert!(big.contains("BH0"));                    // chip strip
        // Degrade: tiny sizes must not panic and must still show the label.
        for (w, h) in [(100, 6), (60, 10), (30, 8), (10, 4), (0, 0)] {
            let _ = c.render(w, h);
        }
    }
```

- [ ] **Step 2: Run — fails.**

- [ ] **Step 3: Implement the compositor.** Rework `render(&self, width, height)`; keep the zero-dim guard and the `Vec<Vec<(char,Color)>>` + `row_to_line` idiom. Structure (all bounds-checked, char-safe):
  - **Title** row (as today) with ` · {arch}` appended when a chip's arch is known.
  - Compute region budget from `height`: reserve title (1). If remaining `>= 8` and width `>= 60`, allocate a **timeline band** (2 rows) at top and a **chip strip** (1 row) at bottom; the middle is the snake+exhaust (left) and swimlanes (right, when width `>= 70`). Below those thresholds, drop regions in the order: timeline → swimlanes → chip strip → snake-only. Encode as simple `if` gates on width/remaining-height; each region only drawn when its min size is met.
  - **Timeline band:** `sparkline(self.tps_history(), inner_w)` for tok/s + the numeric `842`, and a second sparkline of running counts if room. Reuse `tps_history()`.
  - **Snake + exhaust:** the existing snake body draw, plus `exhaust_count(gen_tps)` drifting `»`/`·` glyphs to the right of the head (frame-indexed positions, dim, cleared at 0 tps). Bounds-clamped to the snake region.
  - **Swimlanes:** `lane_segments(running, queue_avg_s, prefill_avg_s, decode_avg_s, lane_width, max_lanes)`; draw each lane on its own row with `value_to_char_intensity`-shaded segments (queue dim, prefill mid, decode bright), labeled `#1 #2 …`; a completion marker column pulsed on `completed_delta`.
  - **Stats fold-in:** the compact one-liner (existing panel content condensed) in the swimlane column's last row(s), incl. `KV`, `TTFT`, `prefix hit%` when present.
  - **Chip strip:** join `format_chip` over `self.chips()` across the bottom row, each shaded by `temp_to_hue`/heat; "no device telemetry" when empty.
  - The prior two-column `panel_rows` may be reused as the stats block when the dedicated swimlane region isn't drawn (narrow) — keep `panel_rows` or fold its content into the stats line; do not leave it dead (remove if unused, or call it in the narrow path).

- [ ] **Step 4: Run the full lib suite + clippy + macOS cross-check — pass.** Manual: `tt-toplike-tui`, `i` on a serving model → timeline + exhaust + swimlanes + chip strip; shrink the terminal → regions drop cleanly. **Commit.**

```bash
git add src/animation/serving_creature.rs
git commit -m "feat(tui): serving dashboard — timeline, token exhaust, swimlanes, silicon strip"
```

---

## Self-Review

**Spec coverage:** throughput timeline → Task 3 `sparkline` + Task 4 history + Task 5 band; token exhaust → Task 3 `exhaust_count` + Task 5 draw; swimlanes (aggregate, stage-ratio, fallback) → Task 3 `lane_segments` + Task 1 stage avgs + Task 5 draw; silicon strip → Task 3 `format_chip`/`ChipReading` + Task 4 telemetry plumbing + Task 5 strip; adaptive collapse order → Task 5 gates; ANSI-shaded loading → Task 2; metrics extensions (stage/prefix/preemption) → Task 1; box-level silicon + aggregate swimlanes honesty → Global Constraints + Tasks 3/5; error handling (absent metrics→0, no telemetry→placeholder/omit, zero-dim, bounded ring) → Tasks 1/3/4/5 + Global Constraints; testing → each task's pure tests + Task 5 compose/degrade. ✓

**Placeholder scan:** pure logic (`journey_hue`, `sparkline`, `lane_segments`, `format_chip`, `exhaust_count`, fold extensions) fully coded + tested. Task 2 loading-shading and Task 5 compositor describe the render against exact anchors + existing `common.rs`/canvas patterns rather than pasting full render bodies (acceptable per "follow existing patterns"). The `_fmt_bytes` import in Task 3 is explicitly flagged to remove if unused. No TBD.

**Type consistency:** `VllmCounters`/`ServingStats` new fields (Task 1) ↔ used by `lane_segments` args + Task 5 draw; `ChipReading` (Task 3) ↔ `SnakeWorld.chips` (Task 4) ↔ `format_chip`/Task 5; `ServingCreature::update(svc, uptime, chips)` (Task 4) ↔ `snake.rs` call site (Task 4) ↔ `[i]` wiring (Task 4); `sparkline`/`lane_segments`/`format_chip`/`exhaust_count` signatures consistent Task 3 def ↔ Task 5 use; `HISTORY_CAP`, `chips()`, `tps_history()` (Task 4) ↔ Task 5. `journey_hue` pub (Task 2).

**Risk note for the executor:** Task 5 is the largest (the compositor). If a region's math is fiddly, land it region-by-region (title+snake first, then timeline, then swimlanes, then chip strip), keeping tests green between — but commit once as Task 5. Reuse `row_to_line` and the zero-dim guard verbatim; never index without a bound check.
