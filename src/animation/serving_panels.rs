// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Pure layout helpers for the serving dashboard's panels — sparkline, request
//! swimlane segments, chip-strip formatting, token-exhaust emission. Kept free
//! of rendering state so each is unit-tested directly; the compositor in
//! `serving_creature.rs` places their output on the canvas.

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
    let temp = c
        .temp_c
        .map(|t| format!("{t:.0}°C"))
        .unwrap_or_else(|| "—".into());
    let pow = c
        .power_w
        .map(|p| format!("{p:.0}W"))
        .unwrap_or_else(|| "—".into());
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
        assert_eq!(
            lanes.len(),
            2,
            "one segment-set per running request, capped"
        );
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
        let c = ChipReading {
            index: 0,
            arch: "Blackhole",
            power_w: Some(92.0),
            temp_c: Some(78.0),
            aiclk_mhz: Some(1350),
        };
        let s = format_chip(&c);
        assert!(s.contains("78") && s.contains("92") && (s.contains("1.35") || s.contains("1350")));
        let none = ChipReading {
            index: 1,
            arch: "Blackhole",
            power_w: None,
            temp_c: None,
            aiclk_mhz: None,
        };
        assert!(format_chip(&none).contains('—'));
    }
    #[test]
    fn exhaust_count_scales_with_tps_and_calm_at_zero() {
        assert_eq!(exhaust_count(0.0), 0);
        assert!(exhaust_count(1000.0) > exhaust_count(100.0));
    }
}
