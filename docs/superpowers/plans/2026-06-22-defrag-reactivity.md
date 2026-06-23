# Defrag Reactivity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Defrag Running phase visually reactive to live hardware signals by adding continuous organic breathing (thermal mood + inference energy) and scatter burst events on power spikes.

**Architecture:** All changes are confined to `src/animation/defrag.rs`. New state fields on `DeviceState` feed into a reworked Running-phase render path. The existing phase state machine, DMA animation, and span coalescer are untouched.

**Tech Stack:** Rust, ratatui Span/Style, existing `lerp`/`hsv_to_rgb`/`temp_to_hue` from `crate::animation::common`.

## Global Constraints

- Only `src/animation/defrag.rs` is modified — no other files touched
- No new dependencies — use the existing pseudo-random arithmetic pattern (`(seed * prime) % modulus`) already used in the file; do not import the `rand` crate into defrag
- `lerp` is available via `use crate::animation::lerp;` (already exported from `crate::animation`)
- Brightness ceiling raised from 0.70 → 0.82; no cell ever reaches white
- Scatter bursts fire only during `Phase::Running`; all other phases unchanged
- Tests live in a `#[cfg(test)] mod tests` block at the bottom of `defrag.rs`

---

### Task 1: Add `ScatterBurst` struct and new `DeviceState` fields

**Files:**
- Modify: `src/animation/defrag.rs:76-105` (after `Channel` struct, before `DeviceState`)
- Modify: `src/animation/defrag.rs:109-156` (`DeviceState` struct and `DeviceState::new`)

**Interfaces:**
- Produces:
  - `struct ScatterBurst { channel: usize, cells: Vec<usize>, ttl: u8, intensity: f32 }`
  - `DeviceState` gains: `thermal_mood: f32`, `inference_energy: f32`, `scatter_bursts: Vec<ScatterBurst>`, `burst_cooldown: u64`

- [ ] **Step 1: Add `ScatterBurst` struct**

Insert after the closing `}` of the `Channel` impl block (after line 105), before `struct DeviceState`:

```rust
/// A brief scatter of bright cells within one GDDR channel, triggered by a power spike.
#[derive(Debug, Clone)]
struct ScatterBurst {
    /// Which GDDR channel this burst belongs to.
    channel: usize,
    /// Pre-randomized column indices within the channel's resident region.
    cells: Vec<usize>,
    /// Frames remaining (starts at 18, ~0.3 s at 60 fps).
    ttl: u8,
    /// Peak intensity derived from spike magnitude: (power_ema/power_ema_slow - 1).clamp(0,1).
    intensity: f32,
}
```

- [ ] **Step 2: Add new fields to `DeviceState` struct**

In the `DeviceState` struct (starting line 109), add four new fields at the end of the struct body:

```rust
    /// Slow EMA of mean GDDR temp, normalized 25°C→0.0, 85°C→1.0.
    /// α=0.005 (~200-frame lag). Drives global palette warmth shift.
    thermal_mood: f32,
    /// Fast signal: excess power above slow baseline, clamped 0..1.
    /// α=0.15 EMA + 0.94× per-frame decay. Lifts brightness/saturation.
    inference_energy: f32,
    /// Active scatter bursts (bounded: at most GDDR_CHANNELS × 12 cells).
    scatter_bursts: Vec<ScatterBurst>,
    /// Frames until next burst spawn is allowed (prevents strobing).
    burst_cooldown: u64,
```

- [ ] **Step 3: Initialize new fields in `DeviceState::new`**

In `DeviceState::new` (currently returns a struct literal ending around line 148), add:

```rust
            thermal_mood: 0.0,
            inference_energy: 0.0,
            scatter_bursts: Vec::new(),
            burst_cooldown: 0,
```

- [ ] **Step 4: Write failing tests**

Add a `#[cfg(test)]` module at the very bottom of `defrag.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_state_new_has_zero_reactive_fields() {
        let ds = DeviceState::new(0);
        assert_eq!(ds.thermal_mood, 0.0);
        assert_eq!(ds.inference_energy, 0.0);
        assert!(ds.scatter_bursts.is_empty());
        assert_eq!(ds.burst_cooldown, 0);
    }

    #[test]
    fn scatter_burst_fields_accessible() {
        let burst = ScatterBurst {
            channel: 2,
            cells: vec![0, 5, 10],
            ttl: 18,
            intensity: 0.5,
        };
        assert_eq!(burst.channel, 2);
        assert_eq!(burst.cells.len(), 3);
        assert_eq!(burst.ttl, 18);
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cd /home/ttuser/code/tt-toplike
cargo test --lib animation::defrag 2>&1 | tail -20
```

Expected: `test tests::device_state_new_has_zero_reactive_fields ... ok` and `test tests::scatter_burst_fields_accessible ... ok`

- [ ] **Step 6: Commit**

```bash
git add src/animation/defrag.rs
git commit -m "feat(defrag): add ScatterBurst struct and reactive DeviceState fields"
```

---

### Task 2: Update loop — continuous signals (thermal_mood + inference_energy)

**Files:**
- Modify: `src/animation/defrag.rs` — `update()` method, `Phase::Running` arm (currently line 378–380, just a comment)

**Interfaces:**
- Consumes: `DeviceState.power_ema`, `DeviceState.power_ema_slow`, `DeviceState.channels[*].temp_ema`, `DeviceState.channels[*].enabled`
- Produces: `DeviceState.thermal_mood` and `DeviceState.inference_energy` updated each frame

- [ ] **Step 1: Write failing test for thermal_mood update**

Add to the `#[cfg(test)]` mod:

```rust
    #[test]
    fn thermal_mood_increases_toward_hot() {
        let mut ds = DeviceState::new(0);
        // Simulate all channels at 85°C (normalized = 1.0)
        for ch in ds.channels.iter_mut() {
            ch.enabled = true;
            ch.temp_ema = 85.0;
        }
        // Run 1000 frames of the update logic
        for _ in 0..1000 {
            let enabled: Vec<_> = ds.channels.iter().filter(|c| c.enabled).collect();
            let mean_temp = enabled.iter().map(|c| c.temp_ema).sum::<f32>() / enabled.len() as f32;
            let mean_norm = ((mean_temp - 25.0) / 60.0).clamp(0.0, 1.0);
            ds.thermal_mood = ds.thermal_mood * 0.995 + mean_norm * 0.005;
        }
        // After 1000 frames at max temp, thermal_mood should be meaningfully above 0
        assert!(ds.thermal_mood > 0.02, "thermal_mood={}", ds.thermal_mood);
    }

    #[test]
    fn inference_energy_rises_on_power_spike() {
        let mut ds = DeviceState::new(0);
        ds.power_ema_slow = 50.0; // baseline
        // Spike power to 60W (20% above baseline)
        ds.power_ema = 60.0;
        for _ in 0..30 {
            let energy_raw = (ds.power_ema - ds.power_ema_slow).max(0.0)
                / ds.power_ema_slow.max(1.0);
            ds.inference_energy = ds.inference_energy * 0.85 + energy_raw * 0.15;
            ds.inference_energy *= 0.94;
        }
        assert!(ds.inference_energy > 0.05, "inference_energy={}", ds.inference_energy);
    }

    #[test]
    fn inference_energy_decays_when_power_drops() {
        let mut ds = DeviceState::new(0);
        ds.power_ema_slow = 50.0;
        ds.inference_energy = 0.8; // pre-loaded high
        ds.power_ema = 50.0; // no spike
        for _ in 0..60 {
            let energy_raw = (ds.power_ema - ds.power_ema_slow).max(0.0)
                / ds.power_ema_slow.max(1.0);
            ds.inference_energy = ds.inference_energy * 0.85 + energy_raw * 0.15;
            ds.inference_energy *= 0.94;
        }
        assert!(ds.inference_energy < 0.05, "inference_energy={}", ds.inference_energy);
    }
```

- [ ] **Step 2: Run tests to confirm they fail (the update logic doesn't exist yet)**

```bash
cargo test --lib animation::defrag 2>&1 | grep -E "FAILED|ok|error"
```

Expected: the three new tests pass (they only exercise local arithmetic, not the update() method), existing tests still pass.

- [ ] **Step 3: Add continuous signal updates to the Running phase arm in `update()`**

Find the `Phase::Running => {` arm inside the `match ds.phase {` block (around line 378). It currently contains only a comment. Replace it with:

```rust
                Phase::Running => {
                    // Continuous organic signals — update every frame.

                    // thermal_mood: slow EMA of mean GDDR temp, normalized 25°C→0, 85°C→1.
                    let enabled_chs: Vec<_> = ds.channels.iter().filter(|c| c.enabled).collect();
                    if !enabled_chs.is_empty() {
                        let mean_temp = enabled_chs.iter().map(|c| c.temp_ema).sum::<f32>()
                            / enabled_chs.len() as f32;
                        let mean_norm = ((mean_temp - 25.0) / 60.0).clamp(0.0, 1.0);
                        ds.thermal_mood = ds.thermal_mood * 0.995 + mean_norm * 0.005;
                    }

                    // inference_energy: fast EMA of excess power above slow baseline.
                    let energy_raw = (ds.power_ema - ds.power_ema_slow).max(0.0)
                        / ds.power_ema_slow.max(1.0);
                    ds.inference_energy = ds.inference_energy * 0.85 + energy_raw * 0.15;
                    ds.inference_energy *= 0.94; // additional decay so it falls after load drops
                }
```

- [ ] **Step 4: Run all tests**

```bash
cargo test --lib animation::defrag 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 5: Verify it compiles cleanly**

```bash
cargo build --features tui 2>&1 | grep -E "^error|warning\[" | head -20
```

Expected: no errors; warnings about unused fields are acceptable at this stage.

- [ ] **Step 6: Commit**

```bash
git add src/animation/defrag.rs
git commit -m "feat(defrag): update thermal_mood and inference_energy each Running frame"
```

---

### Task 3: Update loop — scatter burst spawn and decay

**Files:**
- Modify: `src/animation/defrag.rs` — `update()`, `Phase::Running` arm (just extended in Task 2)

**Interfaces:**
- Consumes: `DeviceState.power_ema`, `DeviceState.power_ema_slow`, `DeviceState.channels`, `DeviceState.burst_cooldown`, `self.frame`
- Produces: `DeviceState.scatter_bursts` populated on power spikes, decayed each frame; `DeviceState.burst_cooldown` ticking down

Note: column count per channel is not available in `update()` — use a fixed upper bound of 200 columns as the randomization space for cell indices. The render path will clamp these against the actual `resident_cells` count, so out-of-range indices simply won't match.

- [ ] **Step 1: Write failing tests for burst spawn and decay**

Add to the `#[cfg(test)]` mod:

```rust
    #[test]
    fn burst_spawns_on_power_spike() {
        let mut ds = DeviceState::new(0);
        ds.power_ema_slow = 50.0;
        ds.power_ema = 56.0; // 12% above slow baseline — above 1.08 threshold
        ds.phase = Phase::Running;
        for ch in ds.channels.iter_mut() {
            ch.enabled = true;
            ch.trained = true;
            ch.fill = 1.0;
        }

        // Simulate the burst spawn logic
        let frame = 100u64;
        if ds.power_ema > ds.power_ema_slow * 1.08 && ds.burst_cooldown == 0 {
            let intensity = ((ds.power_ema / ds.power_ema_slow) - 1.0).clamp(0.0, 1.0);
            for ch_idx in 0..GDDR_CHANNELS {
                let ch = &ds.channels[ch_idx];
                if !ch.enabled || !ch.trained { continue; }
                let resident_cells = (ch.fill * 200.0) as usize;
                if resident_cells == 0 { continue; }
                let n_cells = 5 + (frame.wrapping_mul(7).wrapping_add(ch_idx as u64 * 13)) % 8;
                let cells: Vec<usize> = (0..n_cells)
                    .map(|i| {
                        let seed = frame.wrapping_mul(31)
                            .wrapping_add(ch_idx as u64 * 97)
                            .wrapping_add(i as u64 * 61);
                        (seed % resident_cells as u64) as usize
                    })
                    .collect();
                ds.scatter_bursts.push(ScatterBurst { channel: ch_idx, cells, ttl: 18, intensity });
            }
            ds.burst_cooldown = 30;
        }

        assert!(!ds.scatter_bursts.is_empty(), "expected bursts to spawn");
        assert_eq!(ds.burst_cooldown, 30);
    }

    #[test]
    fn burst_ttl_decays_and_removes() {
        let mut ds = DeviceState::new(0);
        ds.scatter_bursts.push(ScatterBurst {
            channel: 0,
            cells: vec![0, 1, 2],
            ttl: 2,
            intensity: 0.5,
        });

        // Simulate two decay frames
        for _ in 0..2 {
            for b in ds.scatter_bursts.iter_mut() { b.ttl = b.ttl.saturating_sub(1); }
            ds.scatter_bursts.retain(|b| b.ttl > 0);
        }

        assert!(ds.scatter_bursts.is_empty(), "burst should be removed after ttl expires");
    }

    #[test]
    fn burst_cooldown_prevents_double_spawn() {
        let mut ds = DeviceState::new(0);
        ds.power_ema_slow = 50.0;
        ds.power_ema = 60.0;
        ds.burst_cooldown = 10; // cooldown active

        // Should NOT spawn
        let should_spawn = ds.power_ema > ds.power_ema_slow * 1.08 && ds.burst_cooldown == 0;
        assert!(!should_spawn);
    }
```

- [ ] **Step 2: Run tests to confirm they pass (arithmetic tests, no integration with update() yet)**

```bash
cargo test --lib animation::defrag 2>&1 | grep -E "FAILED|ok"
```

Expected: all pass.

- [ ] **Step 3: Add burst spawn + decay to the Running phase arm in `update()`**

Extend the `Phase::Running` arm added in Task 2, appending after the `inference_energy` lines:

```rust
                    // Scatter burst spawn — fires on upward power spike during Running.
                    if ds.burst_cooldown == 0
                        && ds.power_ema > ds.power_ema_slow * 1.08
                        && ds.power_ema_slow > 5.0
                    {
                        let intensity = ((ds.power_ema / ds.power_ema_slow) - 1.0).clamp(0.0, 1.0);
                        for ch_idx in 0..GDDR_CHANNELS {
                            let ch = &ds.channels[ch_idx];
                            if !ch.enabled || !ch.trained { continue; }
                            let resident_cells = ((ch.fill * 200.0) as usize).max(1);
                            let n_cells = 5 + (self.frame.wrapping_mul(7).wrapping_add(ch_idx as u64 * 13)) % 8;
                            let cells: Vec<usize> = (0..n_cells as usize)
                                .map(|i| {
                                    let seed = self.frame.wrapping_mul(31)
                                        .wrapping_add(ch_idx as u64 * 97)
                                        .wrapping_add(i as u64 * 61);
                                    (seed % resident_cells as u64) as usize
                                })
                                .collect();
                            ds.scatter_bursts.push(ScatterBurst { channel: ch_idx, cells, ttl: 18, intensity });
                        }
                        ds.burst_cooldown = 30;
                    }
                    ds.burst_cooldown = ds.burst_cooldown.saturating_sub(1);

                    // Scatter burst decay — decrement TTL and remove expired bursts.
                    for b in ds.scatter_bursts.iter_mut() {
                        b.ttl = b.ttl.saturating_sub(1);
                    }
                    ds.scatter_bursts.retain(|b| b.ttl > 0);
```

- [ ] **Step 4: Run all tests**

```bash
cargo test --lib animation::defrag 2>&1 | tail -20
```

Expected: all pass.

- [ ] **Step 5: Compile check**

```bash
cargo build --features tui 2>&1 | grep "^error" | head -10
```

Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add src/animation/defrag.rs
git commit -m "feat(defrag): spawn and decay scatter bursts on power spikes during Running"
```

---

### Task 4: Render path — reactive hue, brightness, saturation, and burst override

**Files:**
- Modify: `src/animation/defrag.rs` — `render_channel_row()`, the `Phase::Running` match arm (lines ~727–749)

**Interfaces:**
- Consumes:
  - `ds.thermal_mood: f32`
  - `ds.inference_energy: f32`
  - `ds.scatter_bursts: Vec<ScatterBurst>`
  - `base_hue: f32` (already computed in `render_channel_row` as `ch_hue`)
  - `ch.temp_ema: f32`
  - Existing `seg_brightness(col)`, `seg_saturation(col)`, `wave1_period`, `scan_pos`, `mvddq_norm`
  - `lerp` from `crate::animation` (add to the existing `use crate::animation::{hsv_to_rgb, temp_to_hue};` import)
- Produces: updated glyph and style for each cell in the Running phase

- [ ] **Step 1: Add `lerp` to the use statement at the top of defrag.rs**

Find the line (around line 45):
```rust
use crate::animation::{hsv_to_rgb, temp_to_hue};
```
Replace with:
```rust
use crate::animation::{hsv_to_rgb, lerp, temp_to_hue};
```

- [ ] **Step 2: Write a test for the hue blend formula**

Add to the `#[cfg(test)]` mod:

```rust
    #[test]
    fn hue_blend_stays_within_bounds() {
        // base ch0 = 100°, max ch_temp_bias = +40°, max global_mood = +25° → max 165°
        let base_channel_hue = 100.0_f32;
        for temp in [25.0_f32, 55.0, 85.0] {
            for mood in [0.0_f32, 0.5, 1.0] {
                let ch_temp_bias = ((temp - 25.0) / 60.0).clamp(0.0, 1.0) * 40.0;
                let global_mood_shift = mood * 25.0;
                let cell_hue = base_channel_hue + ch_temp_bias + global_mood_shift;
                assert!(cell_hue >= 100.0, "hue={cell_hue} went below base");
                assert!(cell_hue <= 165.0, "hue={cell_hue} exceeded max");
            }
        }
    }

    #[test]
    fn burst_lerp_gives_ambient_at_ttl_zero() {
        // When ttl reaches 0 the retain() removes it, but just before removal (ttl=1),
        // t = (18 - 1) / 18.0 ≈ 0.944 — nearly fully ambient.
        let ttl = 1u8;
        let t = (18 - ttl) as f32 / 18.0;
        let burst_v = 0.88_f32;
        let ambient_v = 0.40_f32;
        let blended = lerp(burst_v, ambient_v, t);
        assert!(blended < burst_v, "should be closer to ambient");
        assert!((blended - ambient_v).abs() < 0.08, "blended={blended} should be near ambient");
    }
```

- [ ] **Step 3: Run tests**

```bash
cargo test --lib animation::defrag 2>&1 | grep -E "FAILED|ok"
```

Expected: all pass.

- [ ] **Step 4: Rewrite the `Phase::Running` arm in `render_channel_row`**

Find the `Phase::Running =>` block inside `render_channel_row` (around line 727). Replace the entire arm (from `Phase::Running => {` to its closing `}`) with:

```rust
                    Phase::Running => {
                        // Three-layer hue blend: channel base + per-channel temp bias + global mood.
                        let ch_temp_bias = ((ch_temp - 25.0) / 60.0).clamp(0.0, 1.0) * 40.0;
                        let global_mood_shift = ds.map(|s| s.thermal_mood).unwrap_or(0.0) * 25.0;
                        let cell_hue = (base_hue + ch_temp_bias + global_mood_shift) % 360.0;

                        let inference_energy = ds.map(|s| s.inference_energy).unwrap_or(0.0);
                        let thermal_mood = ds.map(|s| s.thermal_mood).unwrap_or(0.0);

                        // Check if this cell is inside an active scatter burst for this channel.
                        let burst_hit = ds.and_then(|s| {
                            s.scatter_bursts.iter()
                                .filter(|b| b.channel == ch_idx)
                                .find(|b| b.cells.contains(&col))
                                .map(|b| (b.ttl, b.intensity))
                        });

                        if let Some((ttl, intensity)) = burst_hit {
                            // Burst override: warmer flash in channel's own color family, linear decay.
                            let t = (18u8.saturating_sub(ttl)) as f32 / 18.0;
                            let burst_hue = (base_hue + 20.0) % 360.0;
                            let burst_v = 0.75 + intensity * 0.15;
                            let t1 = self.frame % wave1_period;
                            let w1 = (t1 as f32 / wave1_period as f32 * std::f32::consts::TAU).sin();
                            let ambient_v = (seg_brightness(col) + inference_energy * 0.22 + 0.05 * w1)
                                .clamp(0.18, 0.82);
                            let v = lerp(burst_v, ambient_v, t);
                            let s = lerp(1.0_f32, seg_saturation(col) + thermal_mood * 0.15, t).clamp(0.0, 1.0);
                            let c = hsv_to_rgb(burst_hue, s, v);
                            let glyph = if v > 0.60 { '▓' } else { '▒' };
                            (glyph, Style::default().fg(c))
                        } else {
                            let dist_to_scan = (col as i64 - scan_pos as i64).unsigned_abs() as u64;
                            if dist_to_scan == 0 {
                                let c = hsv_to_rgb(cell_hue, 0.90, 0.65);
                                ('▒', Style::default().fg(c))
                            } else if dist_to_scan == 1 {
                                let c = hsv_to_rgb(cell_hue, 0.85, 0.45);
                                ('░', Style::default().fg(c))
                            } else {
                                let t1 = self.frame % wave1_period;
                                let w1 = (t1 as f32 / wave1_period as f32 * std::f32::consts::TAU).sin();
                                let base_v = seg_brightness(col);
                                let mem_lift = mvddq_norm * 0.12;
                                // inference_energy lifts brightness; thermal_mood lifts saturation.
                                let v = (base_v + inference_energy * 0.22 + 0.05 * w1 + mem_lift)
                                    .clamp(0.18, 0.82);
                                let s = (seg_saturation(col) + thermal_mood * 0.15).clamp(0.0, 1.0);
                                let c = hsv_to_rgb(cell_hue, s, v);
                                let glyph = if v > 0.55 { '▓' } else if v > 0.38 { '▒' } else { '░' };
                                (glyph, Style::default().fg(c))
                            }
                        }
                    }
```

Note: the `(glyph, style)` tuple must be returned from each arm. The existing code assigns `(glyph, style)` then uses them — keep the same pattern.

- [ ] **Step 5: Build and fix any compile errors**

```bash
cargo build --features tui 2>&1 | grep "^error" | head -20
```

Common issues to fix:
- If `col` is not in scope inside the burst check closure, capture it by value: `|b| b.cells.contains(&col)`
- If `ch_idx` shadows an outer variable, use the one already in scope in `render_channel_row`
- The `base_hue` variable is already defined in `render_channel_row` as `ch_hue` (same value, line ~624); use whichever name is in scope

- [ ] **Step 6: Run all tests**

```bash
cargo test --lib animation::defrag 2>&1 | tail -20
```

Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add src/animation/defrag.rs
git commit -m "feat(defrag): reactive hue/brightness/saturation and scatter burst render"
```

---

### Task 5: Reset reactive state on phase transitions

**Files:**
- Modify: `src/animation/defrag.rs` — `update()`, transition side-effects `match` block (around line 320)

**Interfaces:**
- Consumes: phase transition arms
- Produces: `scatter_bursts` cleared and `burst_cooldown` reset when leaving Running; `inference_energy` zeroed when entering Idle or Deconstructing

- [ ] **Step 1: Write tests for state reset on transition**

Add to the `#[cfg(test)]` mod:

```rust
    #[test]
    fn scatter_bursts_clear_on_evict() {
        let mut ds = DeviceState::new(0);
        ds.scatter_bursts.push(ScatterBurst {
            channel: 0, cells: vec![1, 2], ttl: 10, intensity: 0.3,
        });
        ds.inference_energy = 0.7;

        // Simulate the Deconstructing transition side-effect
        ds.scatter_bursts.clear();
        ds.burst_cooldown = 0;
        ds.inference_energy = 0.0;

        assert!(ds.scatter_bursts.is_empty());
        assert_eq!(ds.burst_cooldown, 0);
        assert_eq!(ds.inference_energy, 0.0);
    }
```

- [ ] **Step 2: Run the test**

```bash
cargo test --lib animation::defrag::tests::scatter_bursts_clear_on_evict 2>&1 | tail -10
```

Expected: passes (arithmetic only).

- [ ] **Step 3: Add reset to the transition side-effects block**

Find the `match (ds.phase, new_phase)` block (around line 320). Add to the `(_, Phase::Deconstructing)` arm:

```rust
                (_, Phase::Deconstructing) => {
                    ds.running_since = None;
                    ds.evict_cooldown = 2700;
                    // Clear reactive state — burst list and energy don't survive an eviction.
                    ds.scatter_bursts.clear();
                    ds.burst_cooldown = 0;
                    ds.inference_energy = 0.0;
                }
```

Add to the `(_, Phase::Idle)` arm:

```rust
                (_, Phase::Idle) => {
                    ds.idle_power = ds.power_ema;
                    ds.scatter_bursts.clear();
                    ds.burst_cooldown = 0;
                }
```

- [ ] **Step 4: Build and run all tests**

```bash
cargo build --features tui 2>&1 | grep "^error" | head -10
cargo test --lib animation::defrag 2>&1 | tail -20
```

Expected: no errors; all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/animation/defrag.rs
git commit -m "feat(defrag): clear scatter bursts on phase transitions to Idle/Deconstructing"
```

---

### Task 6: Smoke test — run the TUI and observe the animation

This task has no unit tests. It's a visual verification step.

- [ ] **Step 1: Build the release binary**

```bash
cd /home/ttuser/code/tt-toplike
cargo build --release --features tui 2>&1 | grep "^error" | head -10
cp target/release/tt-toplike-tui ~/.local/bin/tt-toplike-tui
cp target/release/tt-toplike ~/.local/bin/tt-toplike
```

Expected: clean build.

- [ ] **Step 2: Run the TUI in mock mode and switch to Defrag**

```bash
tt-toplike-tui --backend mock
```

Press `d` to enter Defrag mode. Observe:
- Running phase (mock backend simulates activity): the grid should show slight color variation and brightness change compared to the static old version
- If the mock backend triggers a power spike, scatter burst cells should flash briefly

- [ ] **Step 3: Verify no regressions in other modes**

Still in the TUI, cycle through other modes with `v`: Starfield, Memory Flow, Memory Castle, Arcade. Each should work normally.

Press `q` to exit.

- [ ] **Step 4: Commit a final cleanup commit if any minor issues were found and fixed**

```bash
git add src/animation/defrag.rs
git commit -m "fix(defrag): visual smoke test fixes" --allow-empty
```

(Use `--allow-empty` only if no code changes were needed; otherwise commit the actual fixes.)
