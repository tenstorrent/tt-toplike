# Chip Portrait Particle Animation

**Date:** 2026-05-22
**Status:** Approved

## Problem

The chip portrait in the Insights screen is static. Every cell renders once per frame with a fixed character and color, so the display looks frozen even when the chip is under heavy load. The `╔║╚` border is flat DarkGray with no data encoded. The Grid display mode added in an earlier phase made this more obvious — a room full of identical, motionless portraits gives no sense of which chips are working or how hard.

## Goals

1. Teal particles stream inward from DRAM rows toward Tensix interior when the chip is active, representing reads.
2. Pink particles stream outward from Tensix cells toward DRAM rows when the chip is above its idle baseline, representing writes.
3. Particle density and speed are driven by real per-device telemetry — not estimates or hardcoded constants.
4. The `╔║╚` border encodes ASIC temperature using the existing heatmap palette.
5. At idle the portrait is nearly still; under load it is visibly alive.

## Non-Goals

- No per-core L1 fill bars (no per-core data available from tt-smi).
- No changes to Grid display mode (scope removed; Insights auto-column layout already covers the use case).
- No changes to the stats sidebar or footer.
- No new display modes or keybindings.

---

## Design

### 1. Particle Type

Defined in `chip_portrait.rs`:

```rust
#[derive(Clone)]
pub struct Particle {
    pub col:      u8,    // portrait column (0..portrait_cols)
    pub progress: f32,   // 0.0 = DRAM row, 1.0 = opposite DRAM row
    pub from_top: bool,  // true = spawned at row 0, false = row (portrait_rows-1)
    pub kind:     ParticleKind,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ParticleKind { Read, Write }
```

`progress` advances by `speed` each tick. A Read particle starts at 0.0 and travels toward 1.0. A Write particle starts at a random interior progress value and travels toward 0.0 (back to its DRAM row).

**Character by `progress` value** (same field, same lookup, both kinds):

| `progress` | Char | Read meaning | Write meaning |
|------------|------|--------------|---------------|
| 0.0–0.15 | `·` | just left DRAM | arriving at DRAM |
| 0.15–0.40 | `∘` | mid-journey | mid-journey |
| 0.40–1.0 | `○` | deep in Tensix | spawned in interior |

Reads grow from `·` to `○` as they dive inward. Writes shrink from `○` to `·` as they return. No separate table needed — `progress` drives both.

**Colors:** Read = `Color::Rgb(79, 209, 197)` (teal). Write = `Color::Rgb(236, 150, 184)` (pink).

Particles do not overwrite `●` (ETH), `╋` (PCIe), or `▪` (DRAM) cells — those take precedence. Multiple particles on the same cell render the one with higher priority: Write > Read (pink is rarer and should win).

### 2. Telemetry → Animation Mapping

All per-device. Uses `AdaptiveBaseline` (already in `src/animation/baseline.rs`) so thresholds self-calibrate to each device's observed idle rather than hardcoded TDP.

| Signal | What it drives |
|--------|----------------|
| `baseline.power_change(power_w)` | Read spawn rate: 0 particles/tick at idle, up to 8/tick at sustained load |
| `power_change > 0.25` | Write particles enabled (power must be 25%+ above idle baseline) |
| `(power_change − 0.25).max(0.0)` | Write spawn rate: 0 at threshold, up to 6/tick at sustained load |
| `aiclk_mhz / max_aiclk_mhz` | Particle speed: `lerp(12.0, 5.0, aiclk_norm)` ticks to cross half the portrait |
| `gddr_temps[col / 4]` | Per-column DRAM brightness (already in `dram_color_for_col`, unchanged) |
| `ddr_status` bitmask | Only trained columns (bit = 1) can spawn particles; untrained columns are silent |
| `asic_temp_c` | Border color `╔║╚` via `tensix_temp_rgb(asic_temp_c)` |

`max_aiclk_mhz` per architecture: BH = 1000, WH = 800, GS = 700.

### 3. tick_particles()

New function in `chip_portrait.rs`:

```rust
pub fn tick_particles(
    particles: &mut Vec<Particle>,
    telemetry:  &Telemetry,
    smbus:      &SmbusTelemetry,
    arch:       Architecture,
    baseline:   &mut AdaptiveBaseline,
    device_idx: usize,
    tick:       u64,
) {
    // 1. Update baseline with current power
    let power = telemetry.power_w.unwrap_or(0.0);
    let aiclk = telemetry.aiclk_mhz.unwrap_or(0) as f32;
    baseline.update(device_idx, power, 0.0, 0.0, aiclk as u32);

    // 2. Compute derived values
    let power_change  = baseline.power_change(power, device_idx).max(0.0);
    let aiclk_norm    = (aiclk / max_aiclk(arch)).min(1.0);
    let speed         = lerp(1.0 / 12.0, 1.0 / 5.0, aiclk_norm);
    let (portrait_cols, portrait_rows) = portrait_dims(arch);
    let ddr_status    = smbus.ddr_status_u64();  // existing helper

    // 3. Advance existing particles; drop expired ones.
    //    Reads travel 0.0 → 1.0 (DRAM inward).
    //    Writes travel start_progress → 0.0 (interior outward to DRAM).
    particles.retain_mut(|p| match p.kind {
        ParticleKind::Read  => { p.progress += speed; p.progress < 1.0 }
        ParticleKind::Write => { p.progress -= speed; p.progress > 0.0 }
    });

    // 4. Spawn reads (always, when power_change > 0)
    let read_budget = (power_change * 8.0) as u32;
    if read_budget > 0 && tick % (8 / read_budget.max(1) + 1) as u64 == 0 {
        if let Some(col) = trained_random_col(ddr_status, portrait_cols, tick) {
            let from_top = (tick % 2) == 0;
            particles.push(Particle { col, progress: 0.0, from_top, kind: ParticleKind::Read });
        }
    }

    // 5. Spawn writes (only above threshold)
    let write_power = (power_change - 0.25).max(0.0);
    if write_power > 0.0 {
        let write_budget = (write_power * 6.0) as u32;
        if write_budget > 0 && tick % (6 / write_budget.max(1) + 1) as u64 == 0 {
            if let Some(col) = trained_random_col(ddr_status, portrait_cols, tick.wrapping_add(37)) {
                let from_top = (tick % 3) != 0;
                // Write starts at a random interior position and moves outward
                let progress = 0.2 + ((tick % 7) as f32) * 0.08;
                particles.push(Particle { col, progress, from_top, kind: ParticleKind::Write });
            }
        }
    }
}
```

`trained_random_col` uses the `ddr_status` bitmask and `tick` as a cheap PRNG seed to pick a trained column without `rand` dependency.

### 4. build_portrait_lines Update

Signature change:

```rust
pub fn build_portrait_lines(
    device:    &Device,
    telemetry: &Telemetry,
    smbus:     &SmbusTelemetry,
    particles: &[Particle],           // NEW
) -> Vec<Line<'static>>
```

After the cell grid is built (as it is today), a second pass iterates `particles` and overwrites the character at `(col, row)` if the cell is a Tensix cell (not ETH/PCIe/DRAM). Row is computed from `progress` and `from_top`:

```rust
let portrait_row = if p.from_top {
    (p.progress * (portrait_rows as f32 / 2.0)) as usize
} else {
    portrait_rows - 1 - (p.progress * (portrait_rows as f32 / 2.0)) as usize
};
```

### 5. Border Temperature Encoding

In `build_portrait_lines`, the left-border `║` characters and the `╔`/`╚` corners are currently `DarkGray`. Replace with `tensix_temp_rgb(asic_temp_c)` (existing function). This connects the border visually to the portrait's temperature palette so both cold and hot chips are self-evident at a glance.

### 6. State in mod.rs

`AppState` gains:

```rust
portrait_particles: HashMap<usize, Vec<Particle>>,
portrait_tick:      u64,
portrait_baseline:  AdaptiveBaseline,
```

In `render_insights`, before calling `render_device_panels`:

```rust
self.portrait_tick = self.portrait_tick.wrapping_add(1);
for device in &devices {
    let particles = self.portrait_particles.entry(device.index).or_default();
    tick_particles(
        particles, telemetry, smbus, device.architecture,
        &mut self.portrait_baseline, device.index, self.portrait_tick,
    );
}
```

Then `render_stats_panel` (which calls `build_portrait_lines`) passes `&self.portrait_particles[device.index]`.

---

## Files Changed

| File | Change |
|------|--------|
| `src/ui/tui/chip_portrait.rs` | `Particle`, `ParticleKind`, `tick_particles()`, updated `build_portrait_lines` signature, border temp color |
| `src/ui/tui/mod.rs` | `portrait_particles`, `portrait_tick`, `portrait_baseline` in state; tick call in `render_insights`; updated `build_portrait_lines` call sites |

No new files. No changes outside the TUI module. Grid display mode unchanged.

---

## Open Questions

None — all design decisions resolved.
