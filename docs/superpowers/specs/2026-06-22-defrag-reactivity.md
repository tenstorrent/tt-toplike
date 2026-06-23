# Defrag Reactivity Redesign

**Date:** 2026-06-22  
**Status:** Approved

## Problem

Defrag's Running phase looks the same at 30% load as at 90% load. The `seg_brightness` and `seg_saturation` maps are computed from a static hash of device/channel/column indices — no live hardware signal touches them. The one sine wave ticks at a fixed period regardless of what the chip is doing. Other visualizers (Memory Flow, Starfield) have clear emergent rhythm during inference; Defrag just keeps going.

## Goals

- Continuous organic breathing: grid brightness/saturation/hue tracks live hardware signals
- Discrete scatter bursts: randomized cell clusters flash within a channel when a power spike is detected, giving inference a visible beat
- Per-channel hue drifts with GDDR temp; global palette shifts warmer under sustained load
- No changes to Init, Dma, Idle, or Deconstructing phases

## Non-Goals

- Particle simulation or per-cell diffusion fields (Option B rejected: too expensive, blobby)
- Changing the phase state machine or EVICT/surge logic
- Touching the DMA write-head animation or span coalescer

---

## Design

### New State

Two new scalar fields on `DeviceState`:

```rust
/// Slow EMA of mean GDDR temp, normalized: 25°C → 0.0, 85°C → 1.0.
/// Updates every frame with α=0.005 (~200-frame lag). Shifts global palette.
thermal_mood: f32,

/// Fast signal: (power_ema - power_ema_slow) / power_ema_slow, clamped 0..1.
/// EMA α=0.15, additional 0.94× decay per frame so it falls after load drops.
/// Lifts brightness and saturation during active inference.
inference_energy: f32,

/// Active scatter bursts. Short-lived, bounded list.
scatter_bursts: Vec<ScatterBurst>,

/// Frames remaining before next burst is allowed (prevents strobing).
burst_cooldown: u64,
```

`ScatterBurst`:

```rust
struct ScatterBurst {
    channel: usize,
    cells: Vec<usize>,   // column indices, pre-randomized at spawn within resident region
    ttl: u8,             // counts down from 18 (~0.3 s at 60 fps)
    intensity: f32,      // (power_ema / power_ema_slow - 1.0).clamp(0, 1) at spawn time
}
```

### Color System (Running phase only)

**Hue — three-layer blend:**

```
cell_hue = base_channel_hue                          // fixed rainbow ch0=100°…ch7=310°
          + ch_temp_bias                             // per-channel: temp EMA 25°C→0°, 85°C→+40°
          + global_mood_shift                        // thermal_mood 0→1 maps to 0°→+25° shift
```

Maximum total shift: +65°. Always additive, never reverses — channel identity (which row is which) stays readable. A cool idle chip looks identical to today. A hot chip deep in inference looks noticeably warmer across the whole grid.

**Brightness:**

```
v_final = seg_brightness(col)          // existing static floor: 0.28–0.62
         + inference_energy * 0.22    // lifts during active inference (max +0.22)
         + 0.05 * sin(wave)           // existing sine (unchanged)
         .clamp(0.18, 0.82)           // raised ceiling: was 0.70, now 0.82
```

**Saturation:**

```
s_final = seg_saturation(col)          // existing static floor: 0.80–0.90
         + thermal_mood * 0.15        // hotter chip = more vivid colors
         .clamp(0.0, 1.0)
```

**Scatter burst cell override (while TTL > 0):**

```
hue        = base_channel_hue + 20°   // slightly warmer flash in channel's own family
brightness = 0.75 + intensity * 0.15  // 0.75–0.90, no blowout
saturation = 1.0
```

Decays linearly each frame toward ambient value: `lerp(burst_value, ambient_value, (18 - ttl) / 18.0)`.

### Update Loop Additions (Running phase block)

All three steps run every frame while `phase == Phase::Running`:

**Step 1 — Continuous signals:**

`mean_gddr_temp_normalized = ((mean ch_temp_ema across enabled channels) - 25.0) / 60.0).clamp(0, 1)`

```
thermal_mood     = thermal_mood * 0.995 + mean_gddr_temp_normalized * 0.005
energy_raw       = (power_ema - power_ema_slow).max(0.0) / power_ema_slow.max(1.0)
inference_energy = inference_energy * 0.85 + energy_raw * 0.15
inference_energy = inference_energy * 0.94   // additional decay
```

**Step 2 — Burst spawn check:**
```
if power_ema > power_ema_slow * 1.08   // softer than EVICT threshold (1.12)
   && burst_cooldown == 0:
    for each enabled+trained channel:
        spawn ScatterBurst {
            channel,
            cells: pick 5–12 random cols in 0..resident_cells,
            ttl: 18,
            intensity: (power_ema / power_ema_slow - 1.0).clamp(0, 1),
        }
    burst_cooldown = 30
burst_cooldown = burst_cooldown.saturating_sub(1)
```

**Step 3 — Burst decay:**
```
for burst in scatter_bursts:
    burst.ttl -= 1
scatter_bursts.retain(|b| b.ttl > 0)
```

### Render Path

In `render_channel_row`, Running phase block:

1. Before computing glyph/style for a cell, check if `col` appears in any active burst for this channel. If yes, use burst override colors with linear decay toward ambient.
2. Otherwise use the updated `v_final`/`s_final`/`cell_hue` formulas above.
3. The span coalescer already handles variable per-cell colors — no structural change needed.

Burst lookup: iterate `scatter_bursts` filtered by `channel`, check `cells.contains(&col)`. Burst list is bounded (at most `GDDR_CHANNELS` bursts × 12 cells = 96 entries); lookup cost is negligible.

---

## What Stays Unchanged

- `seg_brightness` / `seg_saturation` static hash maps (become floors, not final values)
- Phase state machine: Init, Dma, Idle, Deconstructing
- EVICT / surge-EVICT logic and its 2700-frame cooldown
- DMA write-head sweep, frontier, seek zone
- Power bar, device label, header, footer
- Span coalescer structure

## Expected Visual Result

- At idle/low load: looks identical to today (thermal_mood ≈ 0, inference_energy ≈ 0)
- At sustained inference: grid breathes visibly — brightness rises and falls with token cadence, palette shifts warmer over tens of seconds as GDDR heats
- On each power spike: 5–12 cells per channel flash with a scatter burst, decaying over ~0.3s — gives inference a clear, non-periodic beat
- Channel identity always legible (hue shift is bounded, base rainbow preserved)
