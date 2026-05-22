# Chip Portrait Particle Animation + Animation Profiles Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add teal/pink particle animation to the Insights chip portrait, encode ASIC temperature in the ╔║╚ border, and add an `AnimationProfile` preset system (AnomaliesOnly → Paranoid) with a TOML config file for user-tunable thresholds — all wired universally across Memory Castle, Starfield, Memory Flow, and the new portrait particles.

**Architecture:** A new `src/config.rs` module holds `AnimationProfile` (enum) and `AnimConfig` (struct with `sensitivity` + `max_particles_scale`). `AdaptiveBaseline` gains a `pub sensitivity: f32` field that scales its `power_change()` output, giving all viz automatic profile sensitivity. Particle state (`Vec<Particle>` per device, `u64` tick, `AdaptiveBaseline`) lives as locals in `run_app`. Memory Castle and Memory Flow read `max_particles_scale` only at construction via `new_with_density`.

**Tech Stack:** Rust, Ratatui 0.30, `serde` + `toml = "0.8"` (already in Cargo.toml), `clap` 4.x (already present), `HashMap<usize, Vec<Particle>>` for portrait state.

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `src/config.rs` | **Create** | `AnimationProfile` enum, `AnimConfig` struct, TOML load/save |
| `src/lib.rs` | **Modify** | `pub mod config;` |
| `src/cli.rs` | **Modify** | `--profile` flag on `Cli` struct |
| `src/animation/baseline.rs` | **Modify** | `pub sensitivity: f32` field; scale `power_change()` output |
| `src/animation/memory_castle.rs` | **Modify** | `set_sensitivity()` method |
| `src/animation/starfield.rs` | **Modify** | `set_sensitivity()` method |
| `src/animation/memory_flow.rs` | **Modify** | `set_sensitivity()` method |
| `src/ui/tui/chip_portrait.rs` | **Modify** | `Particle`, `ParticleKind`, `tick_particles()`, update `build_portrait_lines` + `render_chip_portrait` |
| `src/ui/tui/mod.rs` | **Modify** | Portrait state locals in `run_app`; wire `AnimConfig` at viz init; update `render_device_panels` border color |

---

## Task 1: AnimationProfile + AnimConfig + CLI flag

**Files:**
- Create: `src/config.rs`
- Modify: `src/lib.rs` (add `pub mod config;`)
- Modify: `src/cli.rs` (add `--profile`)
- Test: `src/config.rs` (inline `#[cfg(test)]` mod)

### Background

The five presets differ only in two numbers: `sensitivity` (multiplied into every `power_change()` call) and `max_particles_scale` (multiplied into Memory Castle + Memory Flow particle caps). Normal = 1.0 / 1.0. Paranoid = 5.0 / 3.0. Users who want to tweak further can write `~/.config/tt-toplike/config.toml`.

Config file fields mirror `AnimConfig` field names. Missing fields fall back to the preset's defaults. The file is **optional** — if absent, the CLI `--profile` value (default `normal`) governs.

### Steps

- [ ] **Step 1: Write the failing tests**

Add to the bottom of the new `src/config.rs` (before writing the impl):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normal_sensitivity() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Normal);
        assert!((cfg.sensitivity - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_paranoid_sensitivity() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Paranoid);
        assert!((cfg.sensitivity - 5.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_anomalies_only_sensitivity() {
        let cfg = AnimConfig::from_profile(AnimationProfile::AnomaliesOnly);
        assert!((cfg.sensitivity - 0.2).abs() < f32::EPSILON);
    }

    #[test]
    fn test_max_particles_normal() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Normal);
        // Normal: 600 castle, 200 flow
        assert_eq!(cfg.castle_max_particles(), 600);
        assert_eq!(cfg.flow_max_particles(), 200);
    }

    #[test]
    fn test_max_particles_paranoid() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Paranoid);
        // 3× scale → 1800 castle, 600 flow
        assert_eq!(cfg.castle_max_particles(), 1800);
        assert_eq!(cfg.flow_max_particles(), 600);
    }

    #[test]
    fn test_toml_override_sensitivity() {
        let toml = r#"sensitivity = 2.5"#;
        let overrides: AnimConfigOverrides = toml::from_str(toml).unwrap();
        let base = AnimConfig::from_profile(AnimationProfile::Normal);
        let merged = base.merge(overrides);
        assert!((merged.sensitivity - 2.5).abs() < f32::EPSILON);
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```bash
cd /home/ttuser/code/tt-toplike
cargo test --lib config 2>&1 | grep -E "FAILED|error|not found"
```

Expected: compilation errors (module doesn't exist yet).

- [ ] **Step 3: Create `src/config.rs`**

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Animation configuration: profiles and user-tunable thresholds.

use serde::Deserialize;
use std::path::PathBuf;

/// Named animation sensitivity presets.
///
/// Controls how responsive all visualizations are to hardware activity changes.
/// Applies universally to Memory Castle, Starfield, Memory Flow, and chip portrait particles.
#[derive(Debug, Clone, Copy, PartialEq, clap::ValueEnum)]
pub enum AnimationProfile {
    /// Only extreme spikes trigger visible activity (5× power increase needed)
    AnomaliesOnly,
    /// Gentle response; light workloads appear nearly still
    Relaxed,
    /// Balanced default — mirrors observed idle vs. load contrast
    Normal,
    /// Heightened — moderate workloads produce vigorous animation
    Hyper,
    /// Maximum sensitivity; every fluctuation is visible
    Paranoid,
}

impl Default for AnimationProfile {
    fn default() -> Self { Self::Normal }
}

/// Resolved animation configuration applied to all viz objects.
#[derive(Debug, Clone)]
pub struct AnimConfig {
    /// Multiplier applied to every `AdaptiveBaseline::power_change()` result.
    /// 1.0 = Normal; 0.2 = AnomaliesOnly; 5.0 = Paranoid.
    pub sensitivity: f32,

    /// Scale factor for particle caps (multiplied into base counts).
    /// 1.0 = Normal; 3.0 = Paranoid.
    pub max_particles_scale: f32,
}

impl AnimConfig {
    pub fn from_profile(profile: AnimationProfile) -> Self {
        match profile {
            AnimationProfile::AnomaliesOnly => Self { sensitivity: 0.2, max_particles_scale: 0.5 },
            AnimationProfile::Relaxed       => Self { sensitivity: 0.5, max_particles_scale: 0.75 },
            AnimationProfile::Normal        => Self { sensitivity: 1.0, max_particles_scale: 1.0 },
            AnimationProfile::Hyper         => Self { sensitivity: 2.0, max_particles_scale: 2.0 },
            AnimationProfile::Paranoid      => Self { sensitivity: 5.0, max_particles_scale: 3.0 },
        }
    }

    /// Memory Castle max_particles for this config (base: 600).
    pub fn castle_max_particles(&self) -> usize {
        ((600.0 * self.max_particles_scale) as usize).max(20)
    }

    /// Memory Flow max_particles for this config (base: 200).
    pub fn flow_max_particles(&self) -> usize {
        ((200.0 * self.max_particles_scale) as usize).max(10)
    }

    /// Portrait max_particles per device for this config (base: 32).
    pub fn portrait_max_particles(&self) -> usize {
        ((32.0 * self.max_particles_scale) as usize).max(4)
    }

    /// Apply user-provided TOML overrides on top of profile defaults.
    pub fn merge(mut self, overrides: AnimConfigOverrides) -> Self {
        if let Some(v) = overrides.sensitivity        { self.sensitivity = v; }
        if let Some(v) = overrides.max_particles_scale { self.max_particles_scale = v; }
        self
    }
}

/// Partial overrides from `~/.config/tt-toplike/config.toml`.
/// All fields optional — missing = keep profile default.
#[derive(Debug, Default, Deserialize)]
pub struct AnimConfigOverrides {
    pub sensitivity: Option<f32>,
    pub max_particles_scale: Option<f32>,
}

/// Load config overrides from `~/.config/tt-toplike/config.toml`.
/// Returns `Default` silently if the file is absent or unreadable.
pub fn load_config_overrides() -> AnimConfigOverrides {
    config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("tt-toplike").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normal_sensitivity() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Normal);
        assert!((cfg.sensitivity - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_paranoid_sensitivity() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Paranoid);
        assert!((cfg.sensitivity - 5.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_anomalies_only_sensitivity() {
        let cfg = AnimConfig::from_profile(AnimationProfile::AnomaliesOnly);
        assert!((cfg.sensitivity - 0.2).abs() < f32::EPSILON);
    }

    #[test]
    fn test_max_particles_normal() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Normal);
        assert_eq!(cfg.castle_max_particles(), 600);
        assert_eq!(cfg.flow_max_particles(), 200);
    }

    #[test]
    fn test_max_particles_paranoid() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Paranoid);
        assert_eq!(cfg.castle_max_particles(), 1800);
        assert_eq!(cfg.flow_max_particles(), 600);
    }

    #[test]
    fn test_toml_override_sensitivity() {
        let toml = r#"sensitivity = 2.5"#;
        let overrides: AnimConfigOverrides = toml::from_str(toml).unwrap();
        let base = AnimConfig::from_profile(AnimationProfile::Normal);
        let merged = base.merge(overrides);
        assert!((merged.sensitivity - 2.5).abs() < f32::EPSILON);
    }
}
```

- [ ] **Step 4: Add `dirs` dependency and `pub mod config` to lib.rs**

In `Cargo.toml`, under `[dependencies]`, add:
```toml
dirs = "5"
```

In `src/lib.rs`, after the existing `pub mod cli;` line, add:
```rust
pub mod config;
```

- [ ] **Step 5: Add `--profile` flag to `src/cli.rs`**

After the existing `pub mode: Option<VisualizationMode>` field (around line 139), add:

```rust
    /// Animation sensitivity preset
    ///
    /// Controls how responsive visualizations are to hardware activity.
    /// anomalies-only = nearly still except spikes; paranoid = every fluctuation visible.
    /// User can further tune thresholds in ~/.config/tt-toplike/config.toml.
    #[arg(long, value_enum, default_value = "normal")]
    pub profile: crate::config::AnimationProfile,
```

- [ ] **Step 6: Run tests and verify they pass**

```bash
cd /home/ttuser/code/tt-toplike
cargo test --lib config 2>&1 | grep -E "test result|FAILED|error"
```

Expected: `test result: ok. 6 passed`.

- [ ] **Step 7: Build check**

```bash
cargo build --bin tt-toplike-tui --features tui 2>&1 | grep -E "error|warning.*unused" | head -20
```

Expected: compiles; no new errors.

- [ ] **Step 8: Commit**

```bash
git add src/config.rs src/lib.rs src/cli.rs Cargo.toml Cargo.lock
git commit -m "feat(config): AnimationProfile presets + AnimConfig + --profile CLI flag"
```

---

## Task 2: AdaptiveBaseline sensitivity field + viz set_sensitivity()

**Files:**
- Modify: `src/animation/baseline.rs`
- Modify: `src/animation/memory_castle.rs`
- Modify: `src/animation/starfield.rs`
- Modify: `src/animation/memory_flow.rs`
- Modify: `src/ui/tui/mod.rs` (wire at viz init)
- Test: `src/animation/baseline.rs` (inline tests)

### Background

`AdaptiveBaseline.power_change(device_idx, val)` is the single function all viz call to decide animation intensity. Adding `pub sensitivity: f32` (default 1.0) and multiplying the return value by it gives every viz automatic profile support with one change.

Each viz struct wraps its own `AdaptiveBaseline`. We add a `set_sensitivity(f32)` method to each viz that delegates to its inner baseline. In `run_app`, after constructing each viz, we call `set_sensitivity(anim_cfg.sensitivity)`.

### Steps

- [ ] **Step 1: Write the failing test**

In `src/animation/baseline.rs`, inside the existing `#[cfg(test)] mod tests` block (or add one), add:

```rust
#[test]
fn test_sensitivity_scales_power_change() {
    let mut b = AdaptiveBaseline::new();
    b.sensitivity = 2.0;
    // Feed 20 samples of 10.0W to establish baseline
    for _ in 0..20 {
        b.update(0, 10.0, 1.0, 50.0, 800);
    }
    // At 2× sensitivity, a 10% power increase should appear as a 20% change
    let raw_change = b.power_change(0, 11.0); // 10% above baseline
    assert!((raw_change - 0.2).abs() < 0.01,
        "expected ~0.2 with sensitivity=2.0, got {}", raw_change);
}

#[test]
fn test_default_sensitivity_is_one() {
    let b = AdaptiveBaseline::new();
    assert!((b.sensitivity - 1.0).abs() < f32::EPSILON);
}
```

- [ ] **Step 2: Run test to confirm it fails**

```bash
cargo test --lib animation::baseline 2>&1 | grep -E "FAILED|error" | head -10
```

Expected: compile error (`sensitivity` field doesn't exist yet).

- [ ] **Step 3: Add `pub sensitivity: f32` to `AdaptiveBaseline`**

In `src/animation/baseline.rs`, find the `pub struct AdaptiveBaseline` definition (around line 170). Add the field:

```rust
pub struct AdaptiveBaseline {
    /// Per-device baseline trackers
    device_baselines: HashMap<usize, DeviceBaseline>,

    /// Overall system baseline established flag
    all_established: bool,

    /// Sensitivity multiplier applied to all power_change() outputs.
    /// 1.0 = Normal (default). 5.0 = Paranoid. 0.2 = AnomaliesOnly.
    pub sensitivity: f32,
}
```

Update `AdaptiveBaseline::new()` to initialize it:

```rust
pub fn new() -> Self {
    Self {
        device_baselines: HashMap::new(),
        all_established: false,
        sensitivity: 1.0,
    }
}
```

Update `AdaptiveBaseline::power_change()` (around line 245) to scale by sensitivity:

```rust
pub fn power_change(&self, device_idx: usize, current_power: f32) -> f32 {
    self.device_baselines
        .get(&device_idx)
        .map(|b| b.power_change(current_power) * self.sensitivity)
        .unwrap_or(0.0)
}
```

Also update `current_change()` and `temp_change()` in the same way (multiply by `self.sensitivity`):

```rust
pub fn current_change(&self, device_idx: usize, current_current: f32) -> f32 {
    self.device_baselines
        .get(&device_idx)
        .map(|b| b.current_change(current_current) * self.sensitivity)
        .unwrap_or(0.0)
}

pub fn temp_change(&self, device_idx: usize, current_temp: f32) -> f32 {
    self.device_baselines
        .get(&device_idx)
        .map(|b| b.temp_change(current_temp) * self.sensitivity)
        .unwrap_or(0.0)
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test --lib animation::baseline 2>&1 | grep -E "test result|FAILED"
```

Expected: `test result: ok`.

- [ ] **Step 5: Add `set_sensitivity()` to MemoryCastle**

In `src/animation/memory_castle.rs`, find the `impl MemoryCastle` block. After `pub fn new_with_density(...)`, add:

```rust
/// Set the animation sensitivity multiplier. Delegates to the internal AdaptiveBaseline.
/// Call after construction with the user's AnimConfig.sensitivity value.
pub fn set_sensitivity(&mut self, sensitivity: f32) {
    self.baseline.sensitivity = sensitivity;
}
```

- [ ] **Step 6: Add `set_sensitivity()` to HardwareStarfield**

In `src/animation/starfield.rs`, find the `impl HardwareStarfield` block. After `pub fn new(...)`, add:

```rust
pub fn set_sensitivity(&mut self, sensitivity: f32) {
    self.baseline.sensitivity = sensitivity;
}
```

- [ ] **Step 7: Add `set_sensitivity()` to MemoryFlowVis**

In `src/animation/memory_flow.rs`, find the `impl MemoryFlowVis` block. After `pub fn new_with_density(...)`, add:

```rust
pub fn set_sensitivity(&mut self, sensitivity: f32) {
    self.baseline.sensitivity = sensitivity;
}
```

- [ ] **Step 8: Wire AnimConfig at viz init in `run_app`**

In `src/ui/tui/mod.rs`, at the top of `run_app` (after the `let update_interval = ...` line, around line 151), add:

```rust
// Build animation config from CLI profile + user config file overrides
let anim_cfg = {
    use crate::config::{AnimConfig, load_config_overrides};
    AnimConfig::from_profile(cli.profile).merge(load_config_overrides())
};
```

Then find the lazy viz initialization block (`match display_mode` around line 196). After each `viz = Some(...)` assignment, add the sensitivity call:

For starfield (around line 203):
```rust
if starfield.is_none() {
    // ... existing construction code ...
    let mut sf = HardwareStarfield::new(size.width as usize, size.height as usize);
    sf.set_sensitivity(anim_cfg.sensitivity);
    starfield = Some(sf);
}
```

For memory_castle (around line 212):
```rust
if memory_castle.is_none() {
    let mut mc = MemoryCastle::new(size.width as usize, size.height as usize);
    mc.set_sensitivity(anim_cfg.sensitivity);
    memory_castle = Some(mc);
}
```

For memory_flow (around line 221):
```rust
if memory_flow.is_none() {
    let mut mf = MemoryFlowVis::new(size.width as usize, size.height as usize);
    mf.set_sensitivity(anim_cfg.sensitivity);
    memory_flow = Some(mf);
}
```

- [ ] **Step 9: Build and run to verify**

```bash
cargo build --bin tt-toplike-tui --features tui 2>&1 | grep -E "^error"
```

Expected: no errors.

```bash
cargo run --bin tt-toplike-tui --features tui -- --mock --profile paranoid 2>&1 | head -5
```

Expected: TUI launches (quit immediately with q).

- [ ] **Step 10: Commit**

```bash
git add src/animation/baseline.rs src/animation/memory_castle.rs src/animation/starfield.rs src/animation/memory_flow.rs src/ui/tui/mod.rs
git commit -m "feat(animation): AdaptiveBaseline sensitivity field; set_sensitivity() on all viz; wire AnimConfig in run_app"
```

---

## Task 3: Particle type + tick_particles() in chip_portrait.rs

**Files:**
- Modify: `src/ui/tui/chip_portrait.rs`
- Test: inline `#[cfg(test)]` block in `chip_portrait.rs`

### Background

`Particle` lives in `chip_portrait.rs` because it's tightly coupled to `portrait_dims` and the `core_type_*` functions. `tick_particles` advances progress values and spawns new particles, using `AdaptiveBaseline` (passed by `&mut` from the caller's state) and `SmbusTelemetry` for the `ddr_status` bitmask. The `trained_random_col` helper selects a DRAM column using bitwise tricks so we don't pull in a `rand` dependency.

BH DRAM columns: chip cols where `core_type_bh(col, row) == Dram` for row 0 = cols 1–7, 9–15 (excluding ETH col 0, 16 and PCIe col 8).
WH DRAM columns: chip cols where `core_type_wh(col, row) == Dram` for row 1 = cols 0 and 5.

### Steps

- [ ] **Step 1: Write the failing tests**

In `src/ui/tui/chip_portrait.rs`, inside the existing `#[cfg(test)] mod tests` block, add:

```rust
// ── Particle system ───────────────────────────────────────────────────────

#[test]
fn test_particle_read_advances() {
    let mut p = Particle { col: 3, progress: 0.0, from_top: true, kind: ParticleKind::Read };
    let speed = 0.1;
    p.progress += speed;
    assert!((p.progress - 0.1).abs() < f32::EPSILON);
}

#[test]
fn test_particle_write_retreats() {
    let mut p = Particle { col: 3, progress: 0.5, from_top: true, kind: ParticleKind::Write };
    let speed = 0.1;
    p.progress -= speed;
    assert!((p.progress - 0.4).abs() < 0.001);
}

#[test]
fn test_particle_char_dot() {
    assert_eq!(particle_char(0.05), '·');
}

#[test]
fn test_particle_char_ring() {
    assert_eq!(particle_char(0.25), '∘');
}

#[test]
fn test_particle_char_circle() {
    assert_eq!(particle_char(0.5), '○');
}

#[test]
fn test_trained_random_col_bh_returns_dram_col() {
    // BH ddr_status = all columns trained (bits for cols 1-7, 9-15 set)
    // ddr_status_u64 returns a u64 bitmask where bit i set means col i is trained.
    // For BH: 14 DRAM cols → use high bits.
    let (cols, _) = portrait_dims(Architecture::Blackhole);
    let ddr_status: u64 = u64::MAX; // all trained
    let result = trained_random_col(ddr_status, cols, Architecture::Blackhole, 42);
    assert!(result.is_some(), "should return a column when all trained");
    let col = result.unwrap();
    // Col must be a DRAM col in BH: core_type_bh(col, 0) == Dram
    // (row 0 or 11 are DRAM rows for non-ETH, non-PCIe cols)
    let ct = core_type_bh(col, 0);
    assert_eq!(ct, CoreType::Dram, "col {} should be a DRAM col", col);
}

#[test]
fn test_trained_random_col_none_when_no_trained() {
    let (cols, _) = portrait_dims(Architecture::Blackhole);
    let result = trained_random_col(0u64, cols, Architecture::Blackhole, 42);
    assert!(result.is_none(), "should return None when no trained columns");
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```bash
cargo test --lib ui::tui::chip_portrait -- particle 2>&1 | grep -E "FAILED|error" | head -10
```

Expected: compile errors (`Particle`, `particle_char`, `trained_random_col` not found).

- [ ] **Step 3: Add `Particle`, `ParticleKind`, and helpers**

In `src/ui/tui/chip_portrait.rs`, after the `pub fn portrait_dims(...)` function (around line 23), add:

```rust
/// A single animated data-transfer particle in the chip portrait.
#[derive(Clone)]
pub struct Particle {
    /// Portrait column (0..portrait_cols)
    pub col: u8,
    /// 0.0 = DRAM row boundary, 1.0 = opposite DRAM row boundary.
    /// Reads advance toward 1.0; writes retreat toward 0.0.
    pub progress: f32,
    /// true = spawned at row 0 (top DRAM row), false = row (portrait_rows-1)
    pub from_top: bool,
    pub kind: ParticleKind,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ParticleKind {
    /// Teal: DRAM→Tensix read. Spawned at progress=0.0, travels to 1.0.
    Read,
    /// Pink: Tensix→DRAM write. Spawned at random interior progress, retreats to 0.0.
    Write,
}

/// Map a particle's `progress` to a display character.
/// Same lookup for both Read and Write — progress alone determines appearance.
pub fn particle_char(progress: f32) -> char {
    if      progress < 0.15 { '·' }
    else if progress < 0.40 { '∘' }
    else                     { '○' }
}

/// Return the max aiclk (MHz) for the given architecture, used to normalise speed.
fn max_aiclk(arch: Architecture) -> f32 {
    match arch {
        Architecture::Blackhole => 1000.0,
        Architecture::Wormhole  => 800.0,
        _                       => 700.0,
    }
}

/// Pick a random trained DRAM column for the given architecture.
///
/// Uses `ddr_status` bitmask (bit i set = DRAM col index i is trained) and
/// `tick` as a cheap deterministic seed — no `rand` dependency.
///
/// Returns `None` when no trained columns exist.
pub fn trained_random_col(
    ddr_status: u64,
    portrait_cols: usize,
    arch: Architecture,
    tick: u64,
) -> Option<usize> {
    // Collect trained DRAM chip columns
    let dram_cols: Vec<usize> = (0..portrait_cols)
        .filter(|&c| {
            let is_dram = match arch {
                Architecture::Blackhole => core_type_bh(c, 0) == CoreType::Dram,
                _                       => core_type_wh(c, 1) == CoreType::Dram,
            };
            if !is_dram { return false; }
            // Map chip column to a DRAM channel bit index (0-based among DRAM cols)
            let dram_idx = (0..c)
                .filter(|&cc| match arch {
                    Architecture::Blackhole => core_type_bh(cc, 0) == CoreType::Dram,
                    _                       => core_type_wh(cc, 1) == CoreType::Dram,
                })
                .count();
            (ddr_status >> dram_idx) & 1 == 1
        })
        .collect();

    if dram_cols.is_empty() { return None; }

    // Cheap hash of tick to pick a column
    let idx = ((tick.wrapping_mul(0x9e37_79b9_7f4a_7c15)) >> 32) as usize % dram_cols.len();
    Some(dram_cols[idx])
}
```

- [ ] **Step 4: Add `tick_particles()` function**

After `trained_random_col`, add:

```rust
/// Advance particle positions and spawn new ones for a single device.
///
/// # Arguments
/// * `particles` – mutable particle list for this device
/// * `telemetry` – current telemetry snapshot
/// * `smbus` – SMBUS data (for ddr_status bitmask and aiclk)
/// * `arch` – chip architecture (BH / WH / GS)
/// * `baseline` – per-device adaptive baseline (updated in place)
/// * `device_idx` – index within the baseline
/// * `tick` – monotonic frame counter used as PRNG seed
/// * `max_particles` – ceiling from AnimConfig
pub fn tick_particles(
    particles: &mut Vec<Particle>,
    telemetry: &crate::models::Telemetry,
    smbus: Option<&crate::models::SmbusTelemetry>,
    arch: Architecture,
    baseline: &mut crate::animation::baseline::AdaptiveBaseline,
    device_idx: usize,
    tick: u64,
    max_particles: usize,
) {
    use crate::animation::baseline::AdaptiveBaseline;

    let power  = telemetry.power_w();
    let aiclk  = telemetry.aiclk_mhz() as f32;  // returns u32 directly
    let aiclk_norm = (aiclk / max_aiclk(arch)).clamp(0.0, 1.0);

    // Update baseline (baseline.sensitivity already set by AnimConfig wiring)
    baseline.update(device_idx, power, 0.0, 0.0, aiclk);

    let power_change = baseline.power_change(device_idx, power).max(0.0);

    // Speed: lerp 1/12 → 1/5 as aiclk_norm rises (12 ticks/cross at idle, 5 at max)
    let speed = 1.0_f32 / 12.0 + (1.0 / 5.0 - 1.0 / 12.0) * aiclk_norm;

    let (portrait_cols, _portrait_rows) = portrait_dims(arch);
    let ddr_status = smbus.and_then(|s| s.ddr_status_bitmask()).unwrap_or(u64::MAX);

    // 1. Advance existing particles; drop expired ones.
    particles.retain_mut(|p| match p.kind {
        ParticleKind::Read  => { p.progress += speed; p.progress < 1.0 }
        ParticleKind::Write => { p.progress -= speed; p.progress > 0.0 }
    });

    if particles.len() >= max_particles { return; }

    // 2. Spawn reads (when power is above idle baseline)
    let read_budget = (power_change * 8.0) as u32;
    if read_budget > 0 {
        let interval = 8 / read_budget.max(1) + 1;
        if tick % interval as u64 == 0 {
            if let Some(col) = trained_random_col(ddr_status, portrait_cols, arch, tick) {
                let from_top = (tick % 2) == 0;
                particles.push(Particle { col: col as u8, progress: 0.0, from_top, kind: ParticleKind::Read });
            }
        }
    }

    // 3. Spawn writes only when power is ≥25% above idle baseline (active writing)
    let write_power = (power_change - 0.25).max(0.0);
    if write_power > 0.0 && particles.len() < max_particles {
        let write_budget = (write_power * 6.0) as u32;
        if write_budget > 0 {
            let interval = 6 / write_budget.max(1) + 1;
            if tick % interval as u64 == 0 {
                let seed = tick.wrapping_add(37);
                if let Some(col) = trained_random_col(ddr_status, portrait_cols, arch, seed) {
                    let from_top = (tick % 3) != 0;
                    // Write particles start in the interior and retreat outward
                    let progress = 0.2 + ((tick % 7) as f32) * 0.08;
                    particles.push(Particle { col: col as u8, progress, from_top, kind: ParticleKind::Write });
                }
            }
        }
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test --lib ui::tui::chip_portrait -- particle trained_random 2>&1 | grep -E "test result|FAILED"
```

Expected: `test result: ok`.

- [ ] **Step 6: Build check**

```bash
cargo build --bin tt-toplike-tui --features tui 2>&1 | grep "^error"
```

Expected: no errors.

- [ ] **Step 7: Commit**

```bash
git add src/ui/tui/chip_portrait.rs
git commit -m "feat(chip_portrait): Particle type, tick_particles(), trained_random_col()"
```

---

## Task 4: Particle overlay in build_portrait_lines + portrait state in mod.rs + border temp color

**Files:**
- Modify: `src/ui/tui/chip_portrait.rs` — `build_portrait_lines`, `render_chip_portrait`
- Modify: `src/ui/tui/mod.rs` — portrait state locals, `render_device_panels` border color

### Background

`build_portrait_lines` gains a `particles: &[Particle]` parameter. After building the base `Vec<Span>` grid (which already exists), a second pass iterates the particles and overwrites Tensix cells with the particle character and color. ETH (`●`), PCIe (`╋`), and DRAM (`▪`) cells are never overwritten. When two particles land on the same cell, Write wins over Read.

The `╔║╚` border in `render_device_panels` is currently `Color::DarkGray`. Replace it with the ASIC temperature from the device's telemetry using `tensix_temp_rgb`.

Portrait state in `run_app`: a `HashMap<usize, Vec<Particle>>` for particles per device, a `u64` tick counter, and an `AdaptiveBaseline`. These follow the same lazy-init pattern as `starfield` etc.

### Steps

- [ ] **Step 1: Write the failing test**

In `src/ui/tui/chip_portrait.rs`, in the `#[cfg(test)] mod tests` block, add:

```rust
#[test]
fn test_build_portrait_lines_particle_overlay() {
    let device = make_device(Architecture::Blackhole);
    let smbus  = make_smbus(Architecture::Blackhole);
    let telem  = make_telemetry();

    // Spawn a Read particle at column 1 (BH DRAM col), from_top=true, progress=0.5
    // At progress=0.5 the particle is at portrait_row = (0.5 * 12/2) as usize = 3
    let particles = vec![Particle {
        col: 1,
        progress: 0.5,
        from_top: true,
        kind: ParticleKind::Read,
    }];

    let lines = build_portrait_lines(&device, &telem, Some(&smbus), &particles);

    // Row 3, col 1 should be '○' (progress ≥ 0.40)
    let line = &lines[3];
    // Collect chars from spans
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let chars: Vec<char> = text.chars().collect();
    assert_eq!(chars[1], '○', "particle at (col=1, row=3) should be '○', got '{}'", chars[1]);
}

#[test]
fn test_build_portrait_lines_particle_no_overwrite_dram() {
    let device = make_device(Architecture::Blackhole);
    let smbus  = make_smbus(Architecture::Blackhole);
    let telem  = make_telemetry();

    // Place a Read particle that would land on row 0 (DRAM row in BH, col 1)
    // BH row 0 col 1 is a DRAM cell — particle must NOT overwrite it
    let particles = vec![Particle {
        col: 1,
        progress: 0.0, // progress 0.0 → row 0 (DRAM row)
        from_top: true,
        kind: ParticleKind::Read,
    }];

    let lines = build_portrait_lines(&device, &telem, Some(&smbus), &particles);
    let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
    let chars: Vec<char> = text.chars().collect();
    assert_eq!(chars[1], '▪', "DRAM cell must not be overwritten by particle");
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```bash
cargo test --lib ui::tui::chip_portrait -- test_build_portrait_lines_particle 2>&1 | grep -E "FAILED|error" | head -10
```

Expected: compile error (wrong signature).

- [ ] **Step 3: Update `build_portrait_lines` signature and add particle overlay**

In `src/ui/tui/chip_portrait.rs`, change the signature of `build_portrait_lines` from:

```rust
pub fn build_portrait_lines<'a>(
    device: &Device,
    telemetry: &Telemetry,
    smbus: Option<&SmbusTelemetry>,
    tick: u64,
) -> Vec<Line<'a>> {
```

to:

```rust
pub fn build_portrait_lines<'a>(
    device: &Device,
    telemetry: &Telemetry,
    smbus: Option<&SmbusTelemetry>,
    particles: &[Particle],
) -> Vec<Line<'a>> {
```

Remove the `tick` parameter (it was unused in the body; `build_portrait_rows` accepts it but ignores it — pass `0` internally).

Change the call inside `build_portrait_lines` from:
```rust
let rows_strs = build_portrait_rows(device, telemetry, smbus, tick);
```
to:
```rust
let rows_strs = build_portrait_rows(device, telemetry, smbus, 0);
```

At the end of `build_portrait_lines`, after the `rows_strs.into_iter().enumerate().map(...)` closure that builds `spans`, convert to a mutable `Vec<Vec<Span>>` and apply the particle overlay:

Replace the current final line:
```rust
    rows_strs.into_iter().enumerate().map(|(row, row_str)| {
        // ... existing span-building code ...
        Line::from(spans)
    }).collect()
```

with:

```rust
    let (portrait_cols, portrait_rows) = portrait_dims(device.architecture);

    // Build base span grid (rows × cols)
    let mut grid: Vec<Vec<Span<'a>>> = rows_strs.into_iter().enumerate().map(|(row, row_str)| {
        let mut spans: Vec<Span<'a>> = Vec::with_capacity(cols);
        for (col, ch) in row_str.chars().enumerate() {
            let core_type = match device.architecture {
                Architecture::Blackhole => core_type_bh(col, row),
                _                       => core_type_wh(col, row),
            };
            let style = match (core_type, ch) {
                (CoreType::Pcie, _) =>
                    Style::default().fg(Color::Rgb(244, 196, 113)),
                (CoreType::Eth, '●') =>
                    Style::default().fg(Color::Green),
                (CoreType::Eth, _) =>
                    Style::default().fg(Color::DarkGray),
                (CoreType::Dram, _) =>
                    Style::default().fg(dram_color_for_col(col)),
                (CoreType::Tensix, '·') =>
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                (CoreType::Tensix, _) =>
                    Style::default().fg(tensix_color),
            };
            spans.push(Span::styled(ch.to_string(), style));
        }
        spans
    }).collect();

    // Particle overlay — second pass; Write > Read priority
    for p in particles {
        let col = p.col as usize;
        if col >= portrait_cols { continue; }

        let portrait_row = if p.from_top {
            (p.progress * (portrait_rows as f32 / 2.0)) as usize
        } else {
            portrait_rows.saturating_sub(1)
                .saturating_sub((p.progress * (portrait_rows as f32 / 2.0)) as usize)
        };
        if portrait_row >= portrait_rows { continue; }

        // Never overwrite non-Tensix cells
        let core_type = match device.architecture {
            Architecture::Blackhole => core_type_bh(col, portrait_row),
            _                       => core_type_wh(col, portrait_row),
        };
        if core_type != CoreType::Tensix { continue; }

        let ch = particle_char(p.progress);
        let color = match p.kind {
            ParticleKind::Read  => Color::Rgb(79, 209, 197),  // teal
            ParticleKind::Write => Color::Rgb(236, 150, 184), // pink
        };

        // Write wins over Read: only overwrite if cell is not already a Write particle
        let existing_content = &grid[portrait_row][col].content;
        let existing_is_write = existing_content == "·" || existing_content == "∘" || existing_content == "○";
        if matches!(p.kind, ParticleKind::Write) || !existing_is_write {
            grid[portrait_row][col] = Span::styled(ch.to_string(), Style::default().fg(color));
        }
    }

    grid.into_iter().map(Line::from).collect()
```

**Note:** The duplicate `for (col, ch)` loop must be removed — the grid-building closure above replaces the old closure. Make sure the old `spans` vector code is completely replaced by the new `grid` approach.

- [ ] **Step 4: Update `render_chip_portrait` to pass `particles`**

Change the signature from:
```rust
pub fn render_chip_portrait(
    f: &mut Frame,
    area: Rect,
    device: &Device,
    telemetry: &Telemetry,
    smbus: Option<&SmbusTelemetry>,
    tick: u64,
) -> Rect {
```

to:

```rust
pub fn render_chip_portrait(
    f: &mut Frame,
    area: Rect,
    device: &Device,
    telemetry: &Telemetry,
    smbus: Option<&SmbusTelemetry>,
    particles: &[Particle],
) -> Rect {
```

And change the call inside:
```rust
let lines = build_portrait_lines(device, telemetry, smbus, tick);
```
to:
```rust
let lines = build_portrait_lines(device, telemetry, smbus, particles);
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test --lib ui::tui::chip_portrait 2>&1 | grep -E "test result|FAILED"
```

Expected: `test result: ok` (all existing tests + 2 new ones pass).

- [ ] **Step 6: Add portrait state locals to `run_app` in mod.rs**

In `src/ui/tui/mod.rs`, find the state initialization block around line 178 (where `starfield`, `memory_castle` etc. are declared). After those, add:

```rust
// Portrait particle state — per-device particles + shared baseline
use std::collections::HashMap;
use crate::animation::baseline::AdaptiveBaseline;
let mut portrait_particles: HashMap<usize, Vec<crate::ui::tui::chip_portrait::Particle>> = HashMap::new();
let mut portrait_baseline = AdaptiveBaseline::new();
portrait_baseline.sensitivity = anim_cfg.sensitivity;
let mut portrait_tick: u64 = 0;
```

- [ ] **Step 7: Tick particles every frame in the render path**

In `run_app`, find the `tick` computation (around line 283):
```rust
let tick = SystemTime::now()...
```

After that, in the `Insights` display branch where `render_insights` is called, add the particle ticking loop before the `render_insights` call:

```rust
// Tick portrait particles for each device
{
    use crate::ui::tui::chip_portrait::tick_particles;
    portrait_tick = portrait_tick.wrapping_add(1);
    let devices = backend.devices();
    for device in devices.iter() {
        let plist = portrait_particles.entry(device.index).or_default();
        if let Some(telem) = backend.telemetry(device.index) {
            let smbus = backend.smbus_telemetry(device.index);
            tick_particles(
                plist,
                telem,
                smbus,
                device.architecture,
                &mut portrait_baseline,
                device.index,
                portrait_tick,
                anim_cfg.portrait_max_particles(),
            );
        }
    }
}
render_insights(f, backend, &inference_engine, &process_monitor,
    process_cursor, kill_confirm.as_ref(), cli, tick,
    &portrait_particles);
```

- [ ] **Step 8: Update `render_insights` and `render_device_panels` signatures**

There are two `render_insights` variants: the main one under `#[cfg(feature = "linux-procfs")]` (around line 1819) and a stub `render_insights_no_procfs` (around line 1855). Update only the procfs variant — the stub doesn't call `render_device_panels`.

In `src/ui/tui/mod.rs`, update `fn render_insights(...)` under `#[cfg(feature = "linux-procfs")]`:

```rust
fn render_insights(
    f: &mut Frame,
    backend: &Box<dyn TelemetryBackend>,
    engine: &InferenceEngine,
    pm: &ProcessMonitor,
    cursor: usize,
    kill_confirm: Option<&KillConfirmState>,
    _cli: &Cli,
    tick: u64,
    portrait_particles: &std::collections::HashMap<usize, Vec<crate::ui::tui::chip_portrait::Particle>>,
) {
```

Thread `portrait_particles` down into the `render_device_panels` call inside `render_insights`. Update `fn render_device_panels(...)` signature similarly:

```rust
fn render_device_panels(
    f: &mut Frame,
    area: Rect,
    backend: &Box<dyn TelemetryBackend>,
    _engine: &crate::workload::InferenceEngine,
    tick: u64,
    portrait_particles: &std::collections::HashMap<usize, Vec<crate::ui::tui::chip_portrait::Particle>>,
) {
```

Inside `render_device_panels`, change the `render_chip_portrait` call from:
```rust
render_chip_portrait(f, portrait_rect, device, telem, smbus, tick);
```
to:
```rust
let particles = portrait_particles.get(&idx).map(|v| v.as_slice()).unwrap_or(&[]);
render_chip_portrait(f, portrait_rect, device, telem, smbus, particles);
```

- [ ] **Step 9: Replace DarkGray border with temperature color**

In `render_device_panels`, find the two border rendering blocks (around line 2207 and 2227):

```rust
let border_lines: Vec<Line> = (0..panel_h as usize).map(|r| {
    let ch = if r == 0 { "╔" } else if r == panel_h as usize - 1 { "╚" } else { "║" };
    Line::from(Span::styled(ch, Style::default().fg(Color::DarkGray)))
}).collect();
```

Replace with (use `tensix_temp_rgb` from `chip_portrait`):

```rust
let border_color = if let Some(telem) = telemetry {
    let [r, g, b] = crate::ui::tui::chip_portrait::tensix_temp_rgb(telem.temp_c());
    Color::Rgb(r, g, b)
} else {
    Color::DarkGray
};
let border_lines: Vec<Line> = (0..panel_h as usize).map(|r| {
    let ch = if r == 0 { "╔" } else if r == panel_h as usize - 1 { "╚" } else { "║" };
    Line::from(Span::styled(ch, Style::default().fg(border_color)))
}).collect();
```

Apply the same replacement to the stats sidebar border block (the second identical block, around line 2227).

- [ ] **Step 10: Fix all remaining `build_portrait_lines` / `render_chip_portrait` call sites**

Search for any remaining call to `render_chip_portrait` or `build_portrait_lines` with the old `tick` parameter:

```bash
grep -n "render_chip_portrait\|build_portrait_lines" /home/ttuser/code/tt-toplike/src/ui/tui/mod.rs
```

Update each to pass `&[]` (empty slice) if no particles context is available (e.g., Grid mode, if it still calls the portrait).

- [ ] **Step 11: Build and run all tests**

```bash
cargo test --lib 2>&1 | grep -E "test result|FAILED"
cargo build --bin tt-toplike-tui --features tui 2>&1 | grep "^error"
```

Expected: all tests pass, no build errors.

- [ ] **Step 12: Commit**

```bash
git add src/ui/tui/chip_portrait.rs src/ui/tui/mod.rs
git commit -m "feat(insights): particle overlay in build_portrait_lines; temperature border color; portrait state in run_app"
```

---

## Task 5: Wire max_particles_scale to Memory Castle + Memory Flow at construction

**Files:**
- Modify: `src/ui/tui/mod.rs` — change `new()` calls to `new_with_density()` using `anim_cfg`

### Background

Memory Castle and Memory Flow already have `new_with_density(width, height, max_particles[, glyph_count])`. Task 2 wired `sensitivity` via `set_sensitivity()`. Now we use `anim_cfg.castle_max_particles()` and `anim_cfg.flow_max_particles()` at construction so the particle caps also scale with the profile.

Arcade mode constructs Memory Castle and Memory Flow with halved density internally. We leave Arcade's construction unchanged (it handles its own density split) — only the standalone mode constructions change.

### Steps

- [ ] **Step 1: Verify current construction calls**

```bash
grep -n "MemoryCastle::new\|MemoryFlowVis::new" /home/ttuser/code/tt-toplike/src/ui/tui/mod.rs
```

Expected: two `::new(size.width, size.height)` calls.

- [ ] **Step 2: Replace `new()` with `new_with_density()`**

In `src/ui/tui/mod.rs`, find the memory_castle initialization (around line 212):

```rust
if memory_castle.is_none() {
    let mut mc = MemoryCastle::new(size.width as usize, size.height as usize);
    mc.set_sensitivity(anim_cfg.sensitivity);
    memory_castle = Some(mc);
}
```

Replace with:

```rust
if memory_castle.is_none() {
    let glyph_count = ((30.0 * anim_cfg.max_particles_scale) as usize).max(5);
    let mut mc = MemoryCastle::new_with_density(
        size.width as usize,
        size.height as usize,
        anim_cfg.castle_max_particles(),
        glyph_count,
    );
    mc.set_sensitivity(anim_cfg.sensitivity);
    memory_castle = Some(mc);
}
```

Find the memory_flow initialization (around line 221):

```rust
if memory_flow.is_none() {
    let mut mf = MemoryFlowVis::new(size.width as usize, size.height as usize);
    mf.set_sensitivity(anim_cfg.sensitivity);
    memory_flow = Some(mf);
}
```

Replace with:

```rust
if memory_flow.is_none() {
    let mut mf = MemoryFlowVis::new_with_density(
        size.width as usize,
        size.height as usize,
        anim_cfg.flow_max_particles(),
    );
    mf.set_sensitivity(anim_cfg.sensitivity);
    memory_flow = Some(mf);
}
```

- [ ] **Step 3: Write a profile integration test**

In `src/config.rs` test module, add:

```rust
#[test]
fn test_anomalies_only_castle_particles() {
    let cfg = AnimConfig::from_profile(AnimationProfile::AnomaliesOnly);
    // 600 * 0.5 = 300
    assert_eq!(cfg.castle_max_particles(), 300);
}

#[test]
fn test_hyper_flow_particles() {
    let cfg = AnimConfig::from_profile(AnimationProfile::Hyper);
    // 200 * 2.0 = 400
    assert_eq!(cfg.flow_max_particles(), 400);
}
```

- [ ] **Step 4: Run all tests**

```bash
cargo test --lib 2>&1 | grep -E "test result|FAILED"
```

Expected: `test result: ok`.

- [ ] **Step 5: Build all targets and smoke-test**

```bash
cargo build --bin tt-toplike-tui --features tui 2>&1 | grep "^error"
cargo run --bin tt-toplike-tui --features tui -- --mock --profile paranoid --mode castle 2>&1 | head -3
```

Expected: no build errors; TUI launches (quit with q).

```bash
cargo run --bin tt-toplike-tui --features tui -- --mock --profile anomalies-only 2>&1 | head -3
```

Expected: TUI launches; Insights mode shows portraits with dim borders at room temp.

- [ ] **Step 6: Commit**

```bash
git add src/ui/tui/mod.rs src/config.rs
git commit -m "feat(animation): wire max_particles_scale to MemoryCastle + MemoryFlow at construction"
```

---

## Post-Implementation Smoke Checklist

After all tasks are complete, verify end-to-end before calling done:

```bash
# All unit tests pass
cargo test --lib 2>&1 | tail -3

# All five profiles launch without error
for p in anomalies-only relaxed normal hyper paranoid; do
  echo "Testing --profile $p"
  timeout 3 cargo run --bin tt-toplike-tui --features tui -- --mock --profile $p 2>&1 | head -2 || true
done

# Config file override works
mkdir -p ~/.config/tt-toplike
echo 'sensitivity = 3.0' > ~/.config/tt-toplike/config.toml
timeout 3 cargo run --bin tt-toplike-tui --features tui -- --mock 2>&1 | head -2 || true
rm ~/.config/tt-toplike/config.toml

# Portrait particles visible with mock backend at normal profile
# (visual check — launch and observe teal '·∘○' appearing in Insights portrait)
cargo run --bin tt-toplike-tui --features tui -- --mock --profile normal
```
