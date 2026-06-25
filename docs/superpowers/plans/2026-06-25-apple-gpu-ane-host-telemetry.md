# Apple GPU + ANE Host Telemetry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface Apple GPU (via `ioreg`) and Neural Engine power (via the private `IOReport` framework, no sudo) as additional `--host` devices on macOS, so models running on those accelerators show up in the visualizations.

**Architecture:** Two new macOS-only sampler modules (`gpu_macos` = subprocess+parse, `ane_macos` = unsafe IOReport FFI) feed extra `Device`s into the existing `HostBackend`. The multi-device visualizations render them with no changes, reusing the `grid_override`/`channels_override` fields already on this branch. Everything is `#[cfg(target_os = "macos")]`; new dependencies are macOS-target-only so Linux/TT builds are untouched. Every source degrades to "device absent" on failure.

**Tech Stack:** Rust, `ioreg` (CLI), Apple `IOReport` (private framework via FFI), `core-foundation` + `block2` crates (macOS only), `sysinfo` (existing).

## Global Constraints

- **No sudo.** Nothing in this feature may require elevation.
- **macOS-gated.** All new code is `#[cfg(target_os = "macos")]`; all new deps live under `[target.'cfg(target_os = "macos")'.dependencies]`. Linux/TT builds must be byte-for-byte unaffected.
- **Graceful degradation.** Any sampler failure → that device is simply not added; CPU-only `--host` always works. No panics; FFI failures return `None`.
- **Reuse, don't restructure.** Use the existing `Device` struct, its `grid_override: Option<(usize,usize)>` / `channels_override: Option<usize>` fields, and the `TelemetryBackend` trait. No visualization changes.
- **Throttle accelerator sampling to ~1s** (cache between ticks); the UI refreshes at ~100ms and must not spawn `ioreg` or re-subscribe every frame.
- **Zero warnings**, TDD, frequent commits — match the existing repo bar.
- Existing `HostBackend` shape to integrate with (`src/backend/host.rs`): struct fields at lines 131–158; `sample_socket` ends ~line 295; `init()` device-append loop ~324–345; `update()` lines 374–382; trait accessors `devices`/`telemetry`/`smbus_telemetry` lines 384–394. `Telemetry` fields: `voltage, current, power, asic_temperature, aiclk, heartbeat, timestamp`. `Device` constructed as a struct literal with `grid_override`/`channels_override`.

---

### Task 1: GPU `ioreg` parser (`gpu_macos`)

Pure parsing of `ioreg` output into a `GpuSample`. No subprocess in the tested function — the engineer tests the parser against a captured fixture string.

**Files:**
- Create: `src/backend/gpu_macos.rs`
- Modify: `src/backend/mod.rs` (register the module, macOS-gated)
- Test: inline `#[cfg(test)] mod tests` in `src/backend/gpu_macos.rs`

**Interfaces:**
- Produces:
  - `pub struct GpuSample { pub model: String, pub core_count: usize, pub util_pct: f32, pub mem_in_use_bytes: u64, pub mem_alloc_bytes: u64 }`
  - `pub fn parse_ioreg(output: &str) -> Option<GpuSample>`
  - `pub fn sample() -> Option<GpuSample>` (spawns `ioreg`, calls `parse_ioreg`)

- [ ] **Step 1: Register the module (macOS-gated)**

In `src/backend/mod.rs`, add near the other backend module declarations:

```rust
#[cfg(target_os = "macos")]
pub mod gpu_macos; // Apple GPU stats via ioreg (no sudo)
```

- [ ] **Step 2: Write the failing test**

Create `src/backend/gpu_macos.rs` with only the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Captured from `ioreg -rc IOAccelerator` on an Apple M4 Pro (trimmed).
    const FIXTURE: &str = r#"
  | |   "model" = "Apple M4 Pro"
  | |   "gpu-core-count" = 20
  | |   "PerformanceStatistics" = {"In use system memory (driver)"=0,"Alloc system memory"=5043453952,"Tiler Utilization %"=13,"Renderer Utilization %"=24,"Device Utilization %"=27,"In use system memory"=650788864}
"#;

    #[test]
    fn parses_gpu_sample_from_ioreg() {
        let s = parse_ioreg(FIXTURE).expect("should parse");
        assert_eq!(s.model, "Apple M4 Pro");
        assert_eq!(s.core_count, 20);
        assert_eq!(s.util_pct, 27.0);
        assert_eq!(s.mem_in_use_bytes, 650_788_864);
        assert_eq!(s.mem_alloc_bytes, 5_043_453_952);
    }

    #[test]
    fn returns_none_on_garbage() {
        assert!(parse_ioreg("no gpu here").is_none());
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib --features tui gpu_macos 2>&1 | tail -20`
Expected: FAIL — `cannot find function parse_ioreg` / `cannot find type GpuSample`.

- [ ] **Step 4: Write the minimal implementation**

Prepend to `src/backend/gpu_macos.rs` (above the test module):

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Apple GPU statistics via `ioreg` (no sudo, no unsafe).
//!
//! Reads the `IOAccelerator` node's `PerformanceStatistics` dictionary plus the
//! GPU `model` and `gpu-core-count`. Used by `HostBackend` to add a GPU device
//! on macOS so Metal/MPS workloads appear in the visualizations.

use std::process::Command;

/// One sample of Apple GPU stats.
#[derive(Debug, Clone)]
pub struct GpuSample {
    pub model: String,
    pub core_count: usize,
    pub util_pct: f32,
    pub mem_in_use_bytes: u64,
    pub mem_alloc_bytes: u64,
}

/// Extract a quoted string value: `"<key>" = "value"`.
fn find_quoted(output: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\" = \"");
    let start = output.find(&needle)? + needle.len();
    let rest = &output[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Extract an unquoted integer value: `"<key>" = 20`.
fn find_int_assign(output: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\" = ");
    let start = output.find(&needle)? + needle.len();
    let rest = &output[start..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Extract a dict-style integer value inside PerformanceStatistics: `"<key>"=123`.
fn find_dict_int(output: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\"=");
    let start = output.find(&needle)? + needle.len();
    let rest = &output[start..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Parse `ioreg -rc IOAccelerator` output into a [`GpuSample`].
///
/// Returns `None` if the GPU utilization field is absent (not an Apple GPU node
/// or unexpected format) so the caller can skip adding a GPU device.
pub fn parse_ioreg(output: &str) -> Option<GpuSample> {
    let util_pct = find_dict_int(output, "Device Utilization %")? as f32;
    Some(GpuSample {
        model: find_quoted(output, "model").unwrap_or_else(|| "Apple GPU".to_string()),
        core_count: find_int_assign(output, "gpu-core-count").unwrap_or(0) as usize,
        util_pct,
        mem_in_use_bytes: find_dict_int(output, "In use system memory").unwrap_or(0),
        mem_alloc_bytes: find_dict_int(output, "Alloc system memory").unwrap_or(0),
    })
}

/// Spawn `ioreg` and parse one GPU sample. `None` if `ioreg` is missing/errors.
pub fn sample() -> Option<GpuSample> {
    let out = Command::new("ioreg")
        .args(["-r", "-d", "1", "-c", "IOAccelerator"])
        .output()
        .ok()?;
    parse_ioreg(&String::from_utf8_lossy(&out.stdout))
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --lib --features tui gpu_macos 2>&1 | tail -20`
Expected: PASS — both tests ok.

- [ ] **Step 6: Sanity-check against the live machine**

Run: `cargo test --lib --features tui gpu_macos -- --nocapture` then manually:
```bash
cat > /tmp/gpucheck.rs <<'EOF'
fn main() { println!("{:?}", tt_toplike::backend::gpu_macos::sample()); }
EOF
```
(Optional smoke check — skip if not on macOS.) Verify `parse_ioreg` returns `Some` for real `ioreg -rc IOAccelerator` output.

- [ ] **Step 7: Commit**

```bash
git add src/backend/gpu_macos.rs src/backend/mod.rs
git commit -m "feat(host): GPU stats via ioreg parser (macOS)"
```

---

### Task 2: Add the GPU device to `HostBackend`

Append a GPU `Device` (index after the CPU sockets) and map its telemetry, throttled to ~1s.

**Files:**
- Modify: `src/backend/host.rs` (struct fields, `init`, `update`, new helper)
- Test: extend `#[cfg(test)] mod tests` in `src/backend/host.rs` (macOS-gated test)

**Interfaces:**
- Consumes: `crate::backend::gpu_macos::{sample, GpuSample}` (Task 1)
- Produces: a GPU `Device` with `board_type: "apple-gpu"`, `bus_id: "gpu:0"`, `grid_override` from core count, `channels_override: Some(4)`; its telemetry keyed by that device index.

- [ ] **Step 1: Add throttle + gpu-index state to the struct**

In `src/backend/host.rs`, inside `pub struct HostBackend { ... }` (after `core_count`, before `config`):

```rust
    /// Index of the synthetic GPU device, if one was detected (macOS).
    #[cfg(target_os = "macos")]
    gpu_device_idx: Option<usize>,

    /// Last time the accelerators (GPU/ANE) were sampled — throttles ioreg/IOReport.
    #[cfg(target_os = "macos")]
    last_accel_sample: std::time::Instant,
```

Initialize both in `with_config` (in the `HostBackend { ... }` literal, after `config`):

```rust
            #[cfg(target_os = "macos")]
            gpu_device_idx: None,
            #[cfg(target_os = "macos")]
            last_accel_sample: std::time::Instant::now(),
```

- [ ] **Step 2: Write the failing test**

Add to the `tests` module in `src/backend/host.rs`:

```rust
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_host_adds_gpu_device_when_available() {
        let mut backend = HostBackend::new();
        backend.init().unwrap();
        // If this Mac exposes an Apple GPU (it does on Apple Silicon), there must
        // be a device whose board_type is "apple-gpu" with renderable geometry.
        if let Some(gpu) = backend.devices().iter().find(|d| d.board_type == "apple-gpu") {
            let (rows, cols) = gpu.tensix_grid();
            assert!(rows > 0 && cols > 0, "gpu grid must be non-empty");
            assert!(gpu.memory_channels() >= 1);
            assert!(backend.telemetry(gpu.index).is_some(), "gpu telemetry present");
        }
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib --features tui macos_host_adds_gpu_device 2>&1 | tail -20`
Expected: FAIL — no device with `board_type == "apple-gpu"` (test body's `find` is `None`, but the assertion inside never runs, so this test *passes vacuously today*). To make it a real failing test first, temporarily change the `if let` to `let gpu = backend.devices().iter().find(...).expect("expected a GPU device");`. Run → FAIL with "expected a GPU device". (Revert to `if let` in Step 5 so CI on non-Apple-GPU hosts still passes.)

- [ ] **Step 4: Add the GPU sampler helper**

In `src/backend/host.rs`, add this method inside `impl HostBackend` (after `sample_socket`):

```rust
    /// Append a GPU device (macOS) if `ioreg` exposes an Apple GPU. Idempotent:
    /// only adds the device once; later calls just refresh its telemetry.
    #[cfg(target_os = "macos")]
    fn sample_gpu(&mut self) {
        let Some(s) = crate::backend::gpu_macos::sample() else {
            return;
        };

        // Map GPU cores into a near-square grid (same approach as CPU cores).
        let cores = s.core_count.max(1);
        let cols = (cores as f64).sqrt().ceil() as usize;
        let cols = cols.max(1);
        let rows = cores.div_ceil(cols).max(1);

        let idx = match self.gpu_device_idx {
            Some(i) => i,
            None => {
                let i = self.devices.len();
                self.devices.push(Device {
                    index: i,
                    board_type: "apple-gpu".to_string(),
                    bus_id: "gpu:0".to_string(),
                    architecture: Architecture::Unknown,
                    coords: "(gpu,0)".to_string(),
                    firmwares: None,
                    limits: None,
                    pcie_speed: None,
                    pcie_width: None,
                    grid_override: Some((rows, cols)),
                    channels_override: Some(4),
                });
                self.gpu_device_idx = Some(i);
                i
            }
        };

        // Telemetry: utilization → "current" proxy; GPU memory drives DDR bars.
        let mem_total = s.mem_alloc_bytes.max(1);
        let fill_pct = ((s.mem_in_use_bytes as f64 / mem_total as f64) * 100.0).min(100.0) as u32;
        self.telemetry.insert(
            idx,
            Telemetry {
                voltage: None,
                current: Some(s.util_pct / 10.0), // 0..10 A-ish proxy from 0..100%
                power: None,                        // no-sudo ioreg can't read GPU power
                asic_temperature: None,
                aiclk: None,
                heartbeat: Some(1),
                timestamp: Utc::now(),
            },
        );
        self.smbus.insert(
            idx,
            SmbusTelemetry {
                ddr_status: Some(format!("{fill_pct}%")),
                arc0_health: Some("1".to_string()),
                ..SmbusTelemetry::default()
            },
        );
    }
```

- [ ] **Step 5: Call it from `init` and `update` (throttled)**

In `init()`, after the CPU `sample_socket` loop (around line 369, before `Ok(())`):

```rust
        #[cfg(target_os = "macos")]
        self.sample_gpu();
```

In `update()`, replace the body so accelerator sampling is throttled to ~1s:

```rust
    fn update(&mut self) -> BackendResult<()> {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();

        for sock in 0..self.socket_count {
            self.sample_socket(sock);
        }

        #[cfg(target_os = "macos")]
        if self.last_accel_sample.elapsed() >= std::time::Duration::from_secs(1) {
            self.sample_gpu();
            self.last_accel_sample = std::time::Instant::now();
        }
        Ok(())
    }
```

Revert the Step 3 `.expect(...)` back to `if let Some(gpu) = ...` if you changed it.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib --features tui host:: 2>&1 | tail -20`
Expected: PASS (on Apple Silicon the GPU branch is exercised; elsewhere it's vacuous).

- [ ] **Step 7: Manual verification**

Run: `cargo build --bin tt-toplike-tui --features tui && ./target/debug/tt-toplike-tui --host --print 2>&1 | grep -iE "apple-gpu|Devices:"`
Expected: a second device `apple-gpu` listed (on Apple Silicon).

- [ ] **Step 8: Commit**

```bash
git add src/backend/host.rs
git commit -m "feat(host): add Apple GPU as a second device (macOS)"
```

---

### Task 3: ANE power sampler via IOReport FFI (`ane_macos`)

Encapsulate all unsafe IOReport + Core Foundation interop. Public surface is safe: construct a sampler, call `sample()` for ANE watts.

**Files:**
- Modify: `Cargo.toml` (macOS-target deps)
- Create: `src/backend/ane_macos.rs`
- Modify: `src/backend/mod.rs` (register module, macOS-gated)
- Test: inline `#[cfg(test)] mod tests` in `src/backend/ane_macos.rs` (pure math only)

**Interfaces:**
- Produces:
  - `pub struct AneSampler { /* private: subscription + previous sample + instant */ }`
  - `pub fn AneSampler::new() -> Option<AneSampler>` (`None` if IOReport/channels unavailable)
  - `pub fn AneSampler::sample(&mut self) -> Option<f64>` (ANE watts since last call)
  - `pub(crate) fn watts_from_energy_delta(microjoules: u64, secs: f64) -> f64` (pure, tested)

- [ ] **Step 1: Add macOS-target dependencies**

In `Cargo.toml`, under the existing `[target.'cfg(target_os = "linux")'.dependencies]` block, add a macOS block:

```toml
# Apple GPU/ANE telemetry (macOS only, no sudo). core-foundation for CFDictionary
# interop with the private IOReport framework; block2 for its iterate callback.
[target.'cfg(target_os = "macos")'.dependencies]
core-foundation = "0.10"
block2 = "0.5"
```

Run: `cargo build --bin tt-toplike-tui --features tui 2>&1 | tail -5` — expected: builds (deps resolve on macOS; no-op elsewhere).

- [ ] **Step 2: Register the module (macOS-gated)**

In `src/backend/mod.rs`:

```rust
#[cfg(target_os = "macos")]
pub mod ane_macos; // Apple ANE power via private IOReport (no sudo)
```

- [ ] **Step 3: Write the failing test (pure math)**

Create `src/backend/ane_macos.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn energy_delta_to_watts() {
        // 1,000,000 µJ over 1.0 s = 1.0 W.
        assert!((watts_from_energy_delta(1_000_000, 1.0) - 1.0).abs() < 1e-9);
        // Guard divide-by-zero / zero window.
        assert_eq!(watts_from_energy_delta(5_000_000, 0.0), 0.0);
    }
}
```

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test --lib --features tui ane_macos 2>&1 | tail -20`
Expected: FAIL — `cannot find function watts_from_energy_delta`.

- [ ] **Step 5: Implement the FFI sampler**

Prepend to `src/backend/ane_macos.rs`. The IOReport symbols and the "Energy Model" group / "ANE"-named channel follow the documented reverse-engineered pattern used by `macmon` (MIT) — referenced for shape only, not depended on.

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Apple Neural Engine power via Apple's private `IOReport` framework.
//!
//! No sudo required. IOReport reports *energy*; we sample twice and divide the
//! delta by elapsed time to get watts. ANE has no public utilization %, so power
//! is the only no-sudo signal. All unsafe/private-API usage is confined here and
//! every failure path returns `None` (the caller then omits the ANE device).

use core_foundation::base::{CFRelease, CFTypeRef};
use core_foundation::dictionary::CFDictionaryRef;
use core_foundation::string::CFStringRef;
use std::time::Instant;

/// Convert an energy delta (microjoules) over a window (seconds) to watts.
pub(crate) fn watts_from_energy_delta(microjoules: u64, secs: f64) -> f64 {
    if secs <= 0.0 {
        return 0.0;
    }
    (microjoules as f64 / 1_000_000.0) / secs
}

#[allow(non_snake_case)]
#[link(name = "IOReport", kind = "framework")]
unsafe extern "C" {
    fn IOReportCopyChannelsInGroup(
        group: CFStringRef,
        subgroup: CFStringRef,
        a: u64,
        b: u64,
        c: u64,
    ) -> CFDictionaryRef;
    fn IOReportCreateSubscription(
        a: *const std::ffi::c_void,
        desired: CFDictionaryRef,
        subbed_out: *mut CFDictionaryRef,
        flags: u64,
        d: CFTypeRef,
    ) -> *mut std::ffi::c_void;
    fn IOReportCreateSamples(
        sub: *mut std::ffi::c_void,
        chans: CFDictionaryRef,
        a: CFTypeRef,
    ) -> CFDictionaryRef;
    fn IOReportCreateSamplesDelta(
        prev: CFDictionaryRef,
        cur: CFDictionaryRef,
        a: CFTypeRef,
    ) -> CFDictionaryRef;
    // Iterate uses an Objective-C block; see block2 usage in `sum_ane_energy`.
    fn IOReportChannelGetGroup(ch: CFDictionaryRef) -> CFStringRef;
    fn IOReportChannelGetChannelName(ch: CFDictionaryRef) -> CFStringRef;
    fn IOReportSimpleGetIntegerValue(ch: CFDictionaryRef, a: i32) -> i64;
}

/// IOReport subscription + previous energy sample for delta computation.
pub struct AneSampler {
    sub: *mut std::ffi::c_void,
    chans: CFDictionaryRef,
    prev_samples: CFDictionaryRef, // last raw sample dict (owned)
    prev_at: Instant,
}

impl AneSampler {
    /// Subscribe to the Energy Model channels. `None` if IOReport is unavailable
    /// or the subscription can't be created (e.g. a future macOS changes it).
    pub fn new() -> Option<AneSampler> {
        // SAFETY: all pointers checked for null before use; CF objects released on drop.
        unsafe {
            let group = cfstr("Energy Model");
            let chans = IOReportCopyChannelsInGroup(group, std::ptr::null(), 0, 0, 0);
            CFRelease(group as CFTypeRef);
            if chans.is_null() {
                return None;
            }
            let mut subbed: CFDictionaryRef = std::ptr::null();
            let sub = IOReportCreateSubscription(
                std::ptr::null(),
                chans,
                &mut subbed,
                0,
                std::ptr::null(),
            );
            if sub.is_null() {
                return None;
            }
            let prev = IOReportCreateSamples(sub, chans, std::ptr::null());
            if prev.is_null() {
                return None;
            }
            Some(AneSampler {
                sub,
                chans,
                prev_samples: prev,
                prev_at: Instant::now(),
            })
        }
    }

    /// Return ANE watts since the previous call, or `None` on sampling failure.
    pub fn sample(&mut self) -> Option<f64> {
        // SAFETY: see new(); we always pair create/delta with release.
        unsafe {
            let cur = IOReportCreateSamples(self.sub, self.chans, std::ptr::null());
            if cur.is_null() {
                return None;
            }
            let delta = IOReportCreateSamplesDelta(self.prev_samples, cur, std::ptr::null());
            if delta.is_null() {
                CFRelease(cur as CFTypeRef);
                return None;
            }
            let secs = self.prev_at.elapsed().as_secs_f64();
            let energy_uj = sum_ane_energy(delta);
            CFRelease(delta as CFTypeRef);
            CFRelease(self.prev_samples as CFTypeRef);
            self.prev_samples = cur;
            self.prev_at = Instant::now();
            Some(watts_from_energy_delta(energy_uj, secs))
        }
    }
}

impl Drop for AneSampler {
    fn drop(&mut self) {
        // SAFETY: pointers were created by IOReport*; release once.
        unsafe {
            if !self.prev_samples.is_null() {
                CFRelease(self.prev_samples as CFTypeRef);
            }
            if !self.chans.is_null() {
                CFRelease(self.chans as CFTypeRef);
            }
            // `sub` is released by CFRelease as a CFType in IOReport's model.
            if !self.sub.is_null() {
                CFRelease(self.sub as CFTypeRef);
            }
        }
    }
}

/// Sum energy (µJ) across channels whose name contains "ANE" in the delta dict.
/// Uses IOReportIterate via a block2 closure.
unsafe fn sum_ane_energy(delta: CFDictionaryRef) -> u64 {
    use block2::RcBlock;
    let mut total: i64 = 0;
    let total_ptr: *mut i64 = &mut total;
    let cb = RcBlock::new(move |chan: CFDictionaryRef| -> i32 {
        let name = IOReportChannelGetChannelName(chan);
        if !name.is_null() && cfstring_contains(name, "ANE") {
            *total_ptr += IOReportSimpleGetIntegerValue(chan, 0);
        }
        0 // kIOReportIterOk
    });
    extern "C" {
        fn IOReportIterate(samples: CFDictionaryRef, cb: *const std::ffi::c_void) -> i32;
    }
    IOReportIterate(delta, cb.as_ptr() as *const std::ffi::c_void);
    total.max(0) as u64
}

// ---- small Core Foundation helpers ----

unsafe fn cfstr(s: &str) -> CFStringRef {
    use core_foundation::string::CFString;
    let cf = CFString::new(s);
    let r = cf.as_concrete_TypeRef();
    std::mem::forget(cf); // caller releases
    r
}

unsafe fn cfstring_contains(s: CFStringRef, needle: &str) -> bool {
    use core_foundation::string::CFString;
    let owned = CFString::wrap_under_get_rule(s);
    owned.to_string().contains(needle)
}
```

> **Implementer note (verification, not a placeholder):** the exact ANE channel
> name in the "Energy Model" group is matched by substring "ANE", the value
> `macmon` matches on Apple Silicon. Before finishing this task, confirm on the
> dev machine by temporarily logging every `(group, channel_name, value)` inside
> the iterate callback and checking an ANE channel appears under load (run a
> Core ML model). If the substring differs on this chip, adjust the `contains`
> match. If no ANE channel exists, `new()`/`sample()` must still return cleanly
> (the sum is 0 → 0 W, which the caller treats as "present but idle").

- [ ] **Step 6: Run the pure-math test to verify it passes**

Run: `cargo test --lib --features tui ane_macos 2>&1 | tail -20`
Expected: PASS (`energy_delta_to_watts`). The FFI is exercised in Task 4's integration smoke test.

- [ ] **Step 7: Verify it builds and links**

Run: `cargo build --bin tt-toplike-tui --features tui 2>&1 | tail -5`
Expected: links against `IOReport` framework, no warnings.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml src/backend/ane_macos.rs src/backend/mod.rs
git commit -m "feat(host): ANE power sampler via private IOReport (macOS, no sudo)"
```

---

### Task 4: Add the ANE device to `HostBackend`

**Files:**
- Modify: `src/backend/host.rs` (struct field, `init`, `sample_gpu`→rename concept to `sample_accelerators`, telemetry mapping)
- Test: extend `tests` in `src/backend/host.rs`

**Interfaces:**
- Consumes: `crate::backend::ane_macos::AneSampler` (Task 3), GPU helper (Task 2)
- Produces: an ANE `Device` (`board_type: "apple-ane"`, `bus_id: "ane:0"`, `grid_override: Some((4,4))`, `channels_override: Some(1)`) with power-derived telemetry.

- [ ] **Step 1: Add ANE sampler state to the struct**

In `pub struct HostBackend`, add (near the macOS fields from Task 2):

```rust
    /// IOReport-based ANE power sampler (macOS), if available.
    #[cfg(target_os = "macos")]
    ane_sampler: Option<crate::backend::ane_macos::AneSampler>,

    /// Index of the synthetic ANE device, if added.
    #[cfg(target_os = "macos")]
    ane_device_idx: Option<usize>,
```

Initialize in `with_config`:

```rust
            #[cfg(target_os = "macos")]
            ane_sampler: None,
            #[cfg(target_os = "macos")]
            ane_device_idx: None,
```

- [ ] **Step 2: Write the failing test**

Add to `tests` in `src/backend/host.rs`:

```rust
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_host_ane_device_is_well_formed_when_present() {
        let mut backend = HostBackend::new();
        backend.init().unwrap();
        if let Some(ane) = backend.devices().iter().find(|d| d.board_type == "apple-ane") {
            let (rows, cols) = ane.tensix_grid();
            assert!(rows > 0 && cols > 0);
            assert!(ane.memory_channels() >= 1);
            // Power may be 0 W when idle but the telemetry entry must exist.
            assert!(backend.telemetry(ane.index).is_some());
        }
    }
```

- [ ] **Step 3: Run test to verify it fails (or is vacuous)**

Run: `cargo test --lib --features tui macos_host_ane 2>&1 | tail -20`
Expected: passes vacuously until Step 4 wires the device. To force a real fail first, temporarily `.expect("expected ANE device")` (revert in Step 5).

- [ ] **Step 4: Initialize the ANE sampler in `init`**

In `init()`, before the `#[cfg(target_os = "macos")] self.sample_gpu();` line from Task 2:

```rust
        #[cfg(target_os = "macos")]
        {
            self.ane_sampler = crate::backend::ane_macos::AneSampler::new();
        }
```

- [ ] **Step 5: Add the ANE sampling helper and call it**

Add to `impl HostBackend` (after `sample_gpu`):

```rust
    /// Append/refresh an ANE device from IOReport power (macOS). No-op if the
    /// IOReport subscription wasn't available.
    #[cfg(target_os = "macos")]
    fn sample_ane(&mut self) {
        let Some(sampler) = self.ane_sampler.as_mut() else {
            return;
        };
        let Some(watts) = sampler.sample() else {
            return;
        };

        let idx = match self.ane_device_idx {
            Some(i) => i,
            None => {
                let i = self.devices.len();
                self.devices.push(Device {
                    index: i,
                    board_type: "apple-ane".to_string(),
                    bus_id: "ane:0".to_string(),
                    architecture: Architecture::Unknown,
                    coords: "(ane,0)".to_string(),
                    firmwares: None,
                    limits: None,
                    pcie_speed: None,
                    pcie_width: None,
                    // Apple's stated 16-core ANE → 4×4 (nominal, not from an API).
                    grid_override: Some((4, 4)),
                    channels_override: Some(1),
                });
                self.ane_device_idx = Some(i);
                i
            }
        };

        // Synthetic utilization from power vs a nominal ANE ceiling (~8 W) so the
        // viz animates; power carried through directly.
        const NOMINAL_ANE_MAX_W: f64 = 8.0;
        let util = ((watts / NOMINAL_ANE_MAX_W) * 100.0).clamp(0.0, 100.0) as f32;
        self.telemetry.insert(
            idx,
            Telemetry {
                voltage: None,
                current: Some(util / 10.0),
                power: Some(watts as f32),
                asic_temperature: None,
                aiclk: None,
                heartbeat: Some(1),
                timestamp: Utc::now(),
            },
        );
        self.smbus.insert(
            idx,
            SmbusTelemetry {
                arc0_health: Some("1".to_string()),
                ..SmbusTelemetry::default()
            },
        );
    }
```

Call it alongside the GPU sampler in both `init()` (after `sample_gpu`) and the throttled block in `update()`:

```rust
        #[cfg(target_os = "macos")]
        self.sample_ane();
```

(In `update()`, place `self.sample_ane();` inside the existing `if self.last_accel_sample.elapsed() >= 1s { ... }` block, after `self.sample_gpu();`.)

Revert any Step 3 `.expect(...)`.

- [ ] **Step 6: Run tests**

Run: `cargo test --lib --features tui host:: 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 7: Manual verification under load**

Run a Core ML / ANE workload (or any app using the Neural Engine), then:
```bash
./target/debug/tt-toplike-tui --host --print 2>&1 | grep -iE "apple-ane|Power"
```
Expected: an `apple-ane` device with non-zero power while the ANE is active.

- [ ] **Step 8: Commit**

```bash
git add src/backend/host.rs
git commit -m "feat(host): add Apple ANE power device via IOReport (macOS)"
```

---

### Task 5: Isolation tests + README documentation

**Files:**
- Modify: `tests/backend_isolation.rs` (macOS-gated GPU/ANE coverage)
- Modify: `README.md` (`--host` section limitations)
- Test: the integration test itself

**Interfaces:**
- Consumes: `HostBackend` with GPU/ANE devices (Tasks 2, 4), `drive_all_visualizations` helper (existing in `tests/backend_isolation.rs`)

- [ ] **Step 1: Write the macOS isolation test**

Append to `tests/backend_isolation.rs`:

```rust
#[cfg(target_os = "macos")]
#[test]
fn macos_accelerator_devices_render_safely() {
    let mut b = HostBackend::new();
    b.init().expect("host init");
    b.update().expect("host update");

    // Any GPU/ANE devices present must have renderable geometry…
    for d in b.devices() {
        let (rows, cols) = d.tensix_grid();
        assert!(rows > 0 && cols > 0, "{} grid empty", d.board_type);
        assert!(d.memory_channels() >= 1, "{} channels", d.board_type);
    }
    // …and the whole multi-device set survives every visualization.
    drive_all_visualizations(&b);
}
```

- [ ] **Step 2: Run it**

Run: `cargo test --test backend_isolation --features tui 2>&1 | tail -10`
Expected: PASS (all isolation tests, including the new macOS one).

- [ ] **Step 3: Document in README**

In `README.md`, under the `--host` section's per-OS table area, add:

```markdown
On Apple Silicon, `--host` also surfaces the **GPU** (utilization + memory, via
`ioreg`) and the **Neural Engine** (power, via the private IOReport framework) as
extra devices — both **without sudo**. Caveats: ANE shows power-derived activity,
not a true utilization % (no API exposes one); its "16 cores" is Apple's stated
figure used only for the star grid; GPU power/temperature/frequency aren't
available without sudo; and GPU utilization is whole-device, not per-process.
```

- [ ] **Step 4: Full verification**

Run:
```bash
cargo test --lib --tests --features tui 2>&1 | grep -E "test result:"
cargo build --bin tt-toplike-tui --features tui 2>&1 | tail -2
```
Expected: all suites pass; zero warnings.

- [ ] **Step 5: Commit**

```bash
git add tests/backend_isolation.rs README.md
git commit -m "test+docs: Apple GPU/ANE host isolation tests and --host docs"
```

---

## Self-Review

**1. Spec coverage:**
- GPU via ioreg → Tasks 1, 2 ✅
- ANE power via IOReport (no sudo) → Tasks 3, 4 ✅
- macOS-target-only deps; Linux/TT untouched → Task 3 Step 1 (Cargo target block), all code `#[cfg(target_os = "macos")]` ✅
- Reuse `grid_override`/`channels_override`, no viz changes → Tasks 2, 4 ✅
- Throttle ~1s → Task 2 Step 5 / Task 4 Step 5 ✅
- Graceful degradation (device absent on failure) → `let Some(..) else { return }` / `Option` returns throughout ✅
- Field mapping (util→current, GPU mem→DDR, ANE watts→power, grids) → Tasks 2, 4 ✅
- Testing (parser fixture, pure watts math, isolation) → Tasks 1, 3, 5 ✅
- README limitations → Task 5 ✅

**2. Placeholder scan:** No TBD/TODO. The one "implementer note" in Task 3 is an explicit runtime *verification* step for a private-API channel name (with concrete fallback behavior), not a deferred decision.

**3. Type consistency:** `GpuSample`, `parse_ioreg`, `sample()` (Task 1) used identically in Task 2. `AneSampler::new()/sample()`, `watts_from_energy_delta` (Task 3) used identically in Task 4. `Device` literal fields match the struct (`grid_override`, `channels_override`). `Telemetry` fields match the existing struct. Device `board_type` strings (`"apple-gpu"`, `"apple-ane"`) consistent between creation and test lookups.

**Known risk (flagged for execution):** Task 3's IOReport FFI signatures and the ANE channel name are based on the documented `macmon` pattern; they require validation against the live framework during execution (Step 5 note). If `block2`/`core-foundation` minor versions differ, `cargo add` will resolve compatible versions.
