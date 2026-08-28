# tt-smi 6.3.0 gddr_telemetry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Parse tt-smi ≥6.3.0's new per-GDDR-channel `gddr_telemetry` block (training/BIST/harvested/dual-temp/directional-ECC) and flow it into `chip_portrait.rs`, `memory_flow.rs`, `memory_castle.rs`, `starfield.rs`, and the Insights sidebar — each falling back to today's packed-register-derived behavior when the new data is absent (older tt-smi, sysfs-only, luwen, mock without the new fabrication).

**Architecture:** New `GddrChannel`/`GddrTelemetry` structs live on `SmbusTelemetry` (not `Device` — the data is dynamic measurement, not static identity, so it inherits the hybrid backend's existing 10s staleness expiry and `smbus_smooth.rs`'s blend-not-smooth treatment for free). Parsed via the same raw-`serde_json::Value`-passthrough-then-`device_from_json`-adjacent-decode pattern `firmwares`/`limits` already use, with a manual tolerant parser (tt-smi's numeric fields here are quoted strings, same inconsistency `DeviceLimitsRaw` works around). Every visualization consumer checks for the new data first and falls back to its existing packed-bitmask/packed-temp logic when absent — never a hard requirement.

**Tech Stack:** Rust, `serde_json::Value` manual parsing (no new dependencies).

**Spec:** `docs/superpowers/specs/2026-08-28-gddr-telemetry-design.md`

## Global Constraints

- 8 GDDR channels on Blackhole (`gddr_telemetry.channels.len()`), **never** `Architecture::memory_channels()` (returns 12 — real physical channels, a different count for a different purpose; `chip_portrait.rs` already documents this exact 8-vs-12 distinction for the existing `ddr_status` bitmask).
- `gddr_telemetry` lives on `SmbusTelemetry`, not `Device`.
- Numeric fields in the real JSON are quoted strings except `channel` (int) and `harvested`/`enabled` (bool) — parse tolerantly, never via a derived `Deserialize` on `GddrChannel`/`GddrTelemetry` directly.
- A channel entry that fails to parse a required field (`channel`/`harvested`/`enabled` missing or wrong type) is skipped, not fabricated as a zeroed channel — matches `salvage_modern_snapshot`'s "one malformed entry costs only that entry" convention.
- `smbus_smooth.rs`'s `blend()` copies `gddr_telemetry` straight through unconditionally (incoming wins) — never smoothed/averaged (temps are point-in-time, ECC counters are monotonic, pass/fail booleans aren't numeric).
- Every visualization falls back to its current behavior when `gddr_telemetry` is `None` or has an empty `channels` list — nothing regresses on hardware/backends that never produce the new block.
- No version-string gate anywhere (e.g. "requires tt-smi 6.3.0+" in a legend) — purely `Option`-presence-gated, consistent with every other tt-smi-version-gated field in this codebase.

---

## File Structure

- `src/models/telemetry.rs` — `GddrChannel`, `GddrTelemetry` structs; `gddr_telemetry: Option<GddrTelemetry>` field on `SmbusTelemetry`.
- `src/backend/json.rs` — raw passthrough on `TTSMIDeviceRaw`/`TTSMIDeviceJSON`; `parse_gddr_telemetry`/`parse_gddr_channel`; wiring into `parsed_from_json_devices` and the hybrid-backend-facing snapshot path.
- `src/backend/smbus_smooth.rs` — `blend()` copy-through; completeness regression test.
- `src/backend/mock.rs` — synthesized `gddr_telemetry` fabrication; a harvested/BIST-fail scenario variant.
- `src/ui/tui/chip_portrait.rs` — real per-channel training/harvest/BIST/dual-temp in the DRAM cell glyph+color.
- `src/animation/memory_flow.rs` — real per-channel training/harvest/BIST state feeding the existing 0/1/2/error status vocabulary.
- `src/animation/memory_castle.rs` — DDR gate row driven by real per-channel state.
- `src/animation/starfield.rs` — DDR planets driven by real per-channel temp/harvest/BIST.
- `src/ui/tui/mod.rs` — Insights sidebar: upgraded GDDR temp row + new trained/harvested/BIST-fail summary row.

---

### Task 1: Model layer — `GddrChannel`/`GddrTelemetry`

**Files:**
- Modify: `src/models/telemetry.rs` (add structs + field, after the existing `enabled_tensix_col` field at line ~344 and the `GddrTempPair`/`unpack_gddr_temps*` block ending ~line 389)

**Interfaces:**
- Produces: `pub struct GddrChannel { channel: usize, harvested: bool, enabled: bool, training_pass: bool, bist_pass: bool, temp_top: Option<f32>, temp_bottom: Option<f32>, corr_rd: u64, corr_wr: u64, uncorr_rd: u64, uncorr_wr: u64 }`; `pub struct GddrTelemetry { speed: Option<String>, max_temp: Option<f32>, enabled_mask: Option<u32>, channels: Vec<GddrChannel> }`; `SmbusTelemetry.gddr_telemetry: Option<GddrTelemetry>`.

- [ ] **Step 1: Write the failing test**

Add to `src/models/telemetry.rs`'s `#[cfg(test)] mod tests`:

```rust
#[test]
fn gddr_telemetry_defaults_to_none_on_smbus_telemetry() {
    let s = SmbusTelemetry::default();
    assert!(s.gddr_telemetry.is_none());
}

#[test]
fn gddr_channel_and_telemetry_construct_and_compare() {
    let ch = GddrChannel {
        channel: 0,
        harvested: false,
        enabled: true,
        training_pass: true,
        bist_pass: true,
        temp_top: Some(46.0),
        temp_bottom: Some(50.0),
        corr_rd: 0,
        corr_wr: 0,
        uncorr_rd: 0,
        uncorr_wr: 0,
    };
    let g = GddrTelemetry {
        speed: Some("16G".to_string()),
        max_temp: Some(50.0),
        enabled_mask: Some(0xff),
        channels: vec![ch],
    };
    assert_eq!(g.channels.len(), 1);
    assert_eq!(g.channels[0].channel, 0);
    assert_eq!(g, g.clone());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib gddr_telemetry_defaults_to_none_on_smbus_telemetry gddr_channel_and_telemetry_construct_and_compare`
Expected: FAIL to compile — `GddrChannel`/`GddrTelemetry` and the `gddr_telemetry` field don't exist yet.

- [ ] **Step 3: Add the structs and field**

Add to `src/models/telemetry.rs`, after the `tensix_col_harvested` function (ends around line 389, right before the `FirmwaresInfo` doc comment):

```rust
/// One GDDR channel's state, from tt-smi ≥ 6.3.0's `gddr_telemetry.channels[]`.
/// Every numeric field arrives as a quoted string in the real JSON (the same
/// inconsistency `DeviceLimitsRaw` already works around) — parsed tolerantly
/// in `backend/json.rs`, never via a derived `Deserialize` on this struct.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct GddrChannel {
    pub channel: usize,
    pub harvested: bool,
    pub enabled: bool,
    pub training_pass: bool,
    pub bist_pass: bool,
    pub temp_top: Option<f32>,
    pub temp_bottom: Option<f32>,
    pub corr_rd: u64,
    pub corr_wr: u64,
    pub uncorr_rd: u64,
    pub uncorr_wr: u64,
}

/// Device-level GDDR telemetry rollup, from tt-smi ≥ 6.3.0's `gddr_telemetry`
/// block. `None` on older tt-smi (JSON simply lacks the key) or a backend
/// that doesn't produce it (sysfs/hybrid without a live tt-smi reader, luwen).
/// Lives on `SmbusTelemetry`, not `Device`: unlike `firmwares`/`limits` (static
/// device identity, parsed once, never expires), these fields are
/// measurements — temps drift, ECC counters accrue — so this rides the
/// hybrid backend's existing whole-SMBUS-surface staleness expiry instead of
/// living forever.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GddrTelemetry {
    pub speed: Option<String>,
    pub max_temp: Option<f32>,
    pub enabled_mask: Option<u32>,
    pub channels: Vec<GddrChannel>,
}
```

Then add the field to `SmbusTelemetry`, right after `enabled_tensix_col` (the last field before the struct's closing brace at line ~344-345):

```rust
    /// ENABLED_TENSIX_COL: 14-bit mask, one bit per Tensix column (Blackhole).
    /// Bit N clear → Tensix column N is harvested.
    pub enabled_tensix_col: Option<u32>,

    /// Per-channel GDDR training/BIST/harvest/temp/ECC state (tt-smi ≥ 6.3.0).
    /// `None` on older tt-smi or a backend that never produces it. See
    /// `GddrTelemetry`'s doc comment for why this isn't on `Device`.
    pub gddr_telemetry: Option<GddrTelemetry>,
}
```

(Remove the old closing `}` that followed `enabled_tensix_col` and use the
one shown above instead — this is the last field in the struct.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib gddr_telemetry_defaults_to_none_on_smbus_telemetry gddr_channel_and_telemetry_construct_and_compare`
Expected: both PASS.

- [ ] **Step 5: Fix every `SmbusTelemetry` literal this field addition breaks**

Adding a field to `SmbusTelemetry` is a compile error at every struct
literal that doesn't use a `..X::default()` spread. Two confirmed sites need
a manual fix (found via `grep -rn "SmbusTelemetry {" src/` and checking each
block for a `::default()` spread — most already have one and are
unaffected):

1. **`src/backend/mock.rs`**, `generate_smbus_telemetry`'s
   `SmbusTelemetry { ... }` literal (ends around line 417-418, no
   `..default()` spread — this file lists every field explicitly). Add
   `gddr_telemetry: None,` right after `enabled_tensix_col,` for now — Task
   4 replaces this `None` with real fabrication.

2. **`src/backend/luwen.rs`**, `map_smbus`'s `SmbusTelemetry { ... }` literal
   (ends around line 287-292) — this one is **deliberately** field-complete
   with no `..Default::default()` spread, and says so in a comment right
   before its closing brace: *"No `..Default::default()`: every
   SmbusTelemetry field is now mapped explicitly, so adding one upstream
   fails the build here instead of silently defaulting to None on this
   backend."* Respect that convention — do not add a spread here. Instead
   add, right before that existing comment:
   ```rust
        // luwen's Telemetry has no gddr_telemetry-equivalent tag today —
        // absent on both BH and WH until upstream adds one.
        gddr_telemetry: None,
   ```
   This file is behind the `luwen-backend` cargo feature, so it is **not**
   compiled by `cargo test --lib --features tui` alone — you must also run
   `cargo check --features luwen-backend` (or `cargo test --lib --features
   luwen-backend`) to catch this site at all.

Check both are fixed, then confirm no other site needs changes: re-run
`grep -rn "SmbusTelemetry {" src/` and for each hit not already checked
above, verify it contains a `..SomeType::default()` spread somewhere in its
block before the closing brace.

- [ ] **Step 6: Run the full test suite across feature sets**

Run: `cargo test --lib --features tui` (expect all pass), then
`cargo check --features luwen-backend` (expect success — this is the
feature-gated `luwen.rs` site from Step 5, which the first command's
`--features tui` alone does not compile).

- [ ] **Step 7: Commit**

```bash
git add src/models/telemetry.rs src/backend/mock.rs src/backend/luwen.rs
git commit -m "feat(telemetry): add GddrChannel/GddrTelemetry model types"
```

(This commit includes the two `gddr_telemetry: None,` fixes from Step 5
alongside the model addition — they're one atomic compiling change.)

---

### Task 2: JSON parsing — `parse_gddr_telemetry`/`parse_gddr_channel` + wiring

**Files:**
- Modify: `src/backend/json.rs`

**Interfaces:**
- Consumes: `GddrChannel`/`GddrTelemetry` (Task 1), `crate::models::telemetry::parse_hex_or_dec` (existing, `pub(crate) fn parse_hex_or_dec(s: &str) -> Option<u32>`).
- Produces: `fn parse_gddr_telemetry(v: &serde_json::Value) -> Option<GddrTelemetry>` (private); `fn parse_gddr_channel(v: &serde_json::Value) -> Option<GddrChannel>` (private); `TTSMIDeviceRaw.gddr_telemetry: Option<serde_json::Value>`; `TTSMIDeviceJSON.gddr_telemetry: Option<serde_json::Value>`.

- [ ] **Step 1: Write the failing tests**

Add to `src/backend/json.rs`'s `#[cfg(test)] mod tests`:

```rust
// Trimmed from a real tt-smi 6.3.0 `-s` snapshot (verified live against 4x
// Blackhole p300c, 2026-08-27). Every numeric leaf except `channel` is a
// quoted string — that inconsistency is exactly what parse_gddr_channel
// works around.
const GDDR_TELEMETRY_JSON: &str = r#"{
    "speed": "16G",
    "max_temp": "50",
    "enabled_mask": "0xff",
    "channels": [
        {
            "channel": 0,
            "harvested": false,
            "enabled": true,
            "training": "pass",
            "bist": "pass",
            "temp_top": "46",
            "temp_bottom": "50",
            "corr_rd": "0",
            "corr_wr": "0",
            "uncorr_rd": "0",
            "uncorr_wr": "0"
        },
        {
            "channel": 1,
            "harvested": true,
            "enabled": false,
            "training": "fail",
            "bist": "fail",
            "temp_top": "0",
            "temp_bottom": "0",
            "corr_rd": "0",
            "corr_wr": "0",
            "uncorr_rd": "0",
            "uncorr_wr": "0"
        }
    ]
}"#;

#[test]
fn parses_real_gddr_telemetry_shape() {
    let v: serde_json::Value = serde_json::from_str(GDDR_TELEMETRY_JSON).unwrap();
    let g = parse_gddr_telemetry(&v).expect("should parse");
    assert_eq!(g.speed.as_deref(), Some("16G"));
    assert_eq!(g.max_temp, Some(50.0));
    assert_eq!(g.enabled_mask, Some(0xff));
    assert_eq!(g.channels.len(), 2);

    let ch0 = &g.channels[0];
    assert_eq!(ch0.channel, 0);
    assert!(!ch0.harvested);
    assert!(ch0.enabled);
    assert!(ch0.training_pass);
    assert!(ch0.bist_pass);
    assert_eq!(ch0.temp_top, Some(46.0));
    assert_eq!(ch0.temp_bottom, Some(50.0));

    let ch1 = &g.channels[1];
    assert!(ch1.harvested);
    assert!(!ch1.enabled);
    assert!(!ch1.training_pass);
    assert!(!ch1.bist_pass);
}

#[test]
fn absent_gddr_telemetry_key_parses_to_none() {
    // Older tt-smi (< 6.3.0) simply doesn't have this key at all.
    let device_json = r#"{"board_info": {"bus_id": "0000:01:00.0"}}"#;
    let v: serde_json::Value = serde_json::from_str(device_json).unwrap();
    assert!(v.get("gddr_telemetry").is_none());
}

#[test]
fn malformed_channel_entry_is_skipped_not_fabricated() {
    let json = r#"{
        "speed": "16G",
        "channels": [
            {"channel": 0, "harvested": false, "enabled": true, "training": "pass", "bist": "pass"},
            {"channel": "not-a-number", "harvested": false, "enabled": true}
        ]
    }"#;
    let v: serde_json::Value = serde_json::from_str(json).unwrap();
    let g = parse_gddr_telemetry(&v).expect("should still parse the block");
    assert_eq!(g.channels.len(), 1, "the malformed second entry is dropped, not zeroed");
    assert_eq!(g.channels[0].channel, 0);
}

#[test]
fn empty_channels_array_parses_to_empty_vec() {
    let v: serde_json::Value = serde_json::from_str(r#"{"channels": []}"#).unwrap();
    let g = parse_gddr_telemetry(&v).expect("should parse");
    assert!(g.channels.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib parses_real_gddr_telemetry_shape absent_gddr_telemetry_key_parses_to_none malformed_channel_entry_is_skipped_not_fabricated empty_channels_array_parses_to_empty_vec`
Expected: FAIL to compile — `parse_gddr_telemetry`/`parse_gddr_channel` don't exist yet.

- [ ] **Step 3: Add the raw passthrough fields**

In `src/backend/json.rs`, `TTSMIDeviceRaw` (around line 52-59) currently has:

```rust
    pub firmwares: Option<serde_json::Value>,
    /// Thermal/power limits (tt-smi 5.2.0+).
    pub limits: Option<serde_json::Value>,
```

Add immediately after:

```rust
    /// Per-channel GDDR training/BIST/harvest/temp/ECC state (tt-smi 6.3.0+),
    /// raw passthrough — decoded by `parse_gddr_telemetry` at the consuming
    /// site, same pattern as `firmwares`/`limits` above.
    pub gddr_telemetry: Option<serde_json::Value>,
```

Do the same for `TTSMIDeviceJSON` (around line 101-103, the sibling struct
with the identical `firmwares`/`limits` fields).

- [ ] **Step 4: Wire the field through both device-list-building closures**

`parsed_from_json_devices`'s map closure (around line 535) copies
`firmwares: raw.firmwares, limits: raw.limits,` — add
`gddr_telemetry: raw.gddr_telemetry,` right after. The `salvage_modern_snapshot`
per-entry closure (around line 598-599) has the identical
`firmwares: raw.firmwares, limits: raw.limits,` pair — add the same line
there too.

- [ ] **Step 5: Implement the parsers**

Add to `src/backend/json.rs`, near `device_from_json` (before it, so it's
available to use there and in `parsed_from_json_devices`):

```rust
/// Parse tt-smi ≥ 6.3.0's `gddr_telemetry` block. Every numeric leaf is a
/// quoted string in the real JSON except `channel` (already a number) —
/// tolerant of both via `parse_hex_or_dec` and direct `str::parse`. A
/// channel entry that fails to parse a required field is skipped (not
/// fabricated as a zeroed channel), matching `salvage_modern_snapshot`'s
/// "one malformed entry costs only that entry" convention.
fn parse_gddr_telemetry(v: &serde_json::Value) -> Option<crate::models::telemetry::GddrTelemetry> {
    let obj = v.as_object()?;
    let speed = obj.get("speed").and_then(|s| s.as_str()).map(String::from);
    let max_temp = obj
        .get("max_temp")
        .and_then(|s| s.as_str())
        .and_then(|s| s.parse::<f32>().ok());
    let enabled_mask = obj
        .get("enabled_mask")
        .and_then(|s| s.as_str())
        .and_then(crate::models::telemetry::parse_hex_or_dec);
    let channels = obj
        .get("channels")
        .and_then(|c| c.as_array())
        .map(|arr| arr.iter().filter_map(parse_gddr_channel).collect())
        .unwrap_or_default();
    Some(crate::models::telemetry::GddrTelemetry {
        speed,
        max_temp,
        enabled_mask,
        channels,
    })
}

/// Parse one `gddr_telemetry.channels[]` entry. `None` if `channel`,
/// `harvested`, or `enabled` (the three fields with no sane default) are
/// missing or the wrong JSON type — every other field degrades to a default
/// (`false`/`None`/`0`) rather than dropping the whole channel.
fn parse_gddr_channel(v: &serde_json::Value) -> Option<crate::models::telemetry::GddrChannel> {
    let obj = v.as_object()?;
    let str_f32 = |k: &str| obj.get(k)?.as_str()?.parse::<f32>().ok();
    let str_u64 = |k: &str| obj.get(k)?.as_str()?.parse::<u64>().ok();
    let str_bool_pass = |k: &str| obj.get(k).and_then(|v| v.as_str()).map(|s| s == "pass");
    Some(crate::models::telemetry::GddrChannel {
        channel: obj.get("channel")?.as_u64()? as usize,
        harvested: obj.get("harvested")?.as_bool()?,
        enabled: obj.get("enabled")?.as_bool()?,
        training_pass: str_bool_pass("training").unwrap_or(false),
        bist_pass: str_bool_pass("bist").unwrap_or(false),
        temp_top: str_f32("temp_top"),
        temp_bottom: str_f32("temp_bottom"),
        corr_rd: str_u64("corr_rd").unwrap_or(0),
        corr_wr: str_u64("corr_wr").unwrap_or(0),
        uncorr_rd: str_u64("uncorr_rd").unwrap_or(0),
        uncorr_wr: str_u64("uncorr_wr").unwrap_or(0),
    })
}
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib parses_real_gddr_telemetry_shape absent_gddr_telemetry_key_parses_to_none malformed_channel_entry_is_skipped_not_fabricated empty_channels_array_parses_to_empty_vec`
Expected: all 4 PASS.

- [ ] **Step 7: Wire the decoded value into `SmbusTelemetry` at the merge point**

In `src/backend/json.rs`, `parsed_from_json_devices` (around line 739-748)
currently has:

```rust
    for json_dev in json_devices {
        let idx = json_dev.index.unwrap_or(0);
        devices.push(device_from_json(&json_dev));
        if let Some(telem_json) = json_dev.telemetry {
            telemetry.insert(idx, telemetry_from_json(&telem_json));
        }
        if let Some(smbus_json) = json_dev.smbus {
            smbus.insert(idx, smbus_from_json_fields(smbus_json));
        }
    }
```

Replace the `smbus` handling with:

```rust
        let gddr_telemetry = json_dev.gddr_telemetry.as_ref().and_then(parse_gddr_telemetry);
        match json_dev.smbus {
            Some(smbus_json) => {
                let mut s = smbus_from_json_fields(smbus_json);
                s.gddr_telemetry = gddr_telemetry;
                smbus.insert(idx, s);
            }
            None if gddr_telemetry.is_some() => {
                // Phase 26 precedent: a payload that parses independently of
                // smbus_telem's presence must not be dropped just because
                // that sibling block is missing (permission-restricted run,
                // ARC that didn't answer).
                smbus.insert(
                    idx,
                    crate::models::telemetry::SmbusTelemetry {
                        gddr_telemetry,
                        ..Default::default()
                    },
                );
            }
            None => {}
        }
```

There is a second, near-identical call site around line 1077
(`smbus_map.insert(idx, smbus_from_json_fields(smbus_json));`, inside the
hybrid-backend-facing snapshot-building code) — find it and apply the same
change there (the `gddr_telemetry` raw field is available on whatever
per-device JSON struct that function reads from — check whether it's
`TTSMIDeviceJSON` or a different local type before wiring; if it's a
different struct without a `gddr_telemetry` field yet, add the same raw
passthrough field there too, mirroring Step 3).

- [ ] **Step 8: Run the full test suite**

Run: `cargo test --lib --features tui`
Expected: all pass, including the 4 new tests and every existing `json.rs`
test (firmwares/limits parsing, salvage, etc. — unaffected).

- [ ] **Step 9: Commit**

```bash
git add src/backend/json.rs
git commit -m "feat(json): parse tt-smi 6.3.0 gddr_telemetry into SmbusTelemetry"
```

---

### Task 3: `smbus_smooth.rs` — copy-through + completeness guard

**Files:**
- Modify: `src/backend/smbus_smooth.rs`

**Interfaces:**
- Consumes: `GddrTelemetry` (Task 1).

- [ ] **Step 1: Write the failing tests**

Add to `src/backend/smbus_smooth.rs`'s test module:

```rust
#[test]
fn blend_copies_gddr_telemetry_through_unsmoothed() {
    use crate::models::telemetry::{GddrChannel, GddrTelemetry, SmbusTelemetry};

    let ch_prev = GddrChannel {
        channel: 0,
        harvested: false,
        enabled: true,
        training_pass: true,
        bist_pass: true,
        temp_top: Some(40.0),
        temp_bottom: Some(42.0),
        corr_rd: 0,
        corr_wr: 0,
        uncorr_rd: 0,
        uncorr_wr: 0,
    };
    let prev = SmbusTelemetry {
        gddr_telemetry: Some(GddrTelemetry {
            speed: Some("16G".into()),
            max_temp: Some(42.0),
            enabled_mask: Some(0xff),
            channels: vec![ch_prev],
        }),
        ..Default::default()
    };

    let ch_now = GddrChannel {
        temp_top: Some(60.0),
        temp_bottom: Some(62.0),
        corr_rd: 5,
        ..ch_prev
    };
    let incoming = SmbusTelemetry {
        gddr_telemetry: Some(GddrTelemetry {
            speed: Some("16G".into()),
            max_temp: Some(62.0),
            enabled_mask: Some(0xff),
            channels: vec![ch_now],
        }),
        ..Default::default()
    };

    let blended = blend(&prev, incoming.clone());
    // The whole tick's incoming value wins verbatim — not an average with
    // `prev`'s 40/42 readings. Temps are point-in-time, ECC counters are
    // monotonic; neither is meaningful smoothed.
    assert_eq!(blended.gddr_telemetry, incoming.gddr_telemetry);
    assert_eq!(
        blended.gddr_telemetry.unwrap().channels[0].temp_top,
        Some(60.0),
        "must be this tick's reading, not an EMA toward the previous 40.0"
    );
}

/// Regression guard for the exact bug class Phase 25/26 already hit twice:
/// a field added to `SmbusTelemetry` that `blend()` forgets to touch.
/// Constructing this literal with NO `..Default::default()` forces every
/// field to be named — if a future field is added to `SmbusTelemetry` but
/// not handled by `blend()`, THIS TEST STOPS COMPILING until it's added
/// here too, turning a silent runtime bug into a compile error.
#[test]
fn blend_touches_every_smbus_telemetry_field() {
    use crate::models::telemetry::SmbusTelemetry;

    let incoming = SmbusTelemetry {
        board_id: Some("b".into()),
        ddr_status: Some("0x2".into()),
        ddr_speed: Some("16G".into()),
        arc0_health: Some(1),
        aiclk: Some(1000),
        axiclk: Some(1000),
        arcclk: Some(1000),
        vcore: Some("0.8".into()),
        tdp: Some("100".into()),
        tdc: Some("100".into()),
        asic_temperature: Some("50".into()),
        vreg_temperature: Some("50".into()),
        board_temperature: Some("40".into()),
        eth_fw_version: Some("1.0".into()),
        m3_app_fw_version: Some("1.0".into()),
        m3_bl_fw_version: Some("1.0".into()),
        tt_flash_version: Some("1.0".into()),
        fan_speed: Some(1000),
        pcie_status: Some("Gen4 x16".into()),
        board_power_limit: Some("300".into()),
        therm_trip_count: Some("0".into()),
        vdd_limits: Some("0.7-0.9".into()),
        eth_status0: Some("0".into()),
        eth_status1: Some("0".into()),
        input_power: Some("100".into()),
        thm_limits: Some("90".into()),
        boot_date: Some("2026-01-01".into()),
        rt_seconds: Some("100".into()),
        wh_fw_date: Some("2026-01-01".into()),
        asic_tmon0: Some("50".into()),
        asic_tmon1: Some("50".into()),
        mvddq_power: Some("5".into()),
        gddr_train_temp0: Some("40".into()),
        gddr_train_temp1: Some("40".into()),
        aux_status: Some("0".into()),
        eth_debug_status0: Some("0".into()),
        eth_debug_status1: Some("0".into()),
        gddr_temps: [None, None, None, None],
        max_gddr_temp: Some(50.0),
        gddr_corr_errs: [None, None, None, None],
        gddr_uncorr_errs: Some(0),
        harvesting_state: Some(0),
        eth_live_status: Some(0),
        enabled_eth: Some(0),
        enabled_gddr: Some(0xff),
        enabled_l2cpu: Some(0),
        enabled_tensix_col: Some(0x3fff),
        gddr_telemetry: Some(crate::models::telemetry::GddrTelemetry::default()),
    };
    let prev = SmbusTelemetry::default();
    let blended = blend(&prev, incoming.clone());
    assert_eq!(
        blended, incoming,
        "blend() must copy or correctly merge every field — if this \
         assertion fails after adding a new SmbusTelemetry field, blend() \
         is silently dropping it"
    );
}
```

(This test's exact field list must match `SmbusTelemetry`'s current fields
exactly — if `cargo build` reports a missing or unknown field in this
literal, that tells you exactly what to add/remove; do NOT add
`..Default::default()` to this literal, that defeats its whole purpose.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib blend_copies_gddr_telemetry_through_unsmoothed blend_touches_every_smbus_telemetry_field`
Expected: FAIL to compile (`gddr_telemetry` field doesn't exist on the struct
literal path yet if Task 1 wasn't merged first — but assuming Tasks run in
order, this should fail because `blend()` doesn't yet copy `gddr_telemetry`,
so `blend_touches_every_smbus_telemetry_field` fails at the `assert_eq!`
runtime check, not a compile error, since the struct literal itself compiles
fine once Task 1 landed. Confirm you see a runtime assertion failure, not a
compile error, before proceeding — if it's a compile error, Task 1 wasn't
completed correctly; stop and check.)

- [ ] **Step 3: Add `gddr_telemetry` to `blend()`**

Find `blend()`'s (or `apply_ema`'s — check which function name this file
actually uses; the plan's spec doc calls it `blend()` per the codebase's own
established terminology, verify against the file) field-by-field handling of
`enabled_gddr`/`harvesting_state`/similar bitmask fields (these are already
copied straight through — "copy, don't smooth"). Add, in the same style:

```rust
        gddr_telemetry: incoming.gddr_telemetry.clone(),
```

(If the function returns a new struct via a literal rather than mutating
`prev` in place, add this as one more field in that literal, positioned
alongside the other GDDR fields for readability.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib blend_copies_gddr_telemetry_through_unsmoothed blend_touches_every_smbus_telemetry_field`
Expected: both PASS.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test --lib --features tui`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add src/backend/smbus_smooth.rs
git commit -m "feat(smbus_smooth): copy gddr_telemetry through unsmoothed, add completeness guard"
```

---

### Task 4: Mock backend fabrication

**Files:**
- Modify: `src/backend/mock.rs`

**Interfaces:**
- Consumes: `GddrChannel`/`GddrTelemetry` (Task 1).
- Produces: a synthesized `gddr_telemetry` on the mock's `SmbusTelemetry` output; a scenario variant exercising harvested + BIST-fail.

- [ ] **Step 1: Write the failing test**

Add to `src/backend/mock.rs`'s test module, right after the existing
`test_mock_quad_galaxy_some_harvesting` test (which this mirrors exactly —
same constructor, same iteration style):

```rust
#[test]
fn quad_galaxy_scenario_produces_harvested_and_bist_failed_gddr_channels() {
    let mut b = MockBackend::with_scenario(MockScenario::QuadGalaxy);
    b.init().unwrap();

    let any_gddr_harvested = b.devices().iter().any(|d| {
        b.smbus_telemetry(d.index)
            .and_then(|s| s.gddr_telemetry.as_ref())
            .is_some_and(|g| g.channels.iter().any(|c| c.harvested))
    });
    let any_bist_failed = b.devices().iter().any(|d| {
        b.smbus_telemetry(d.index)
            .and_then(|s| s.gddr_telemetry.as_ref())
            .is_some_and(|g| g.channels.iter().any(|c| !c.bist_pass))
    });
    assert!(any_gddr_harvested, "QuadGalaxy mock should harvest a GDDR channel");
    assert!(any_bist_failed, "QuadGalaxy mock should fail BIST on a GDDR channel");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib quad_galaxy_scenario_produces_harvested_and_bist_failed_gddr_channels`
Expected: FAIL — an assertion failure (no `gddr_telemetry` fabricated yet;
the test should compile fine since `MockBackend::with_scenario`,
`b.smbus_telemetry(idx)`, and `b.init()` all already exist and are used
identically by `test_mock_quad_galaxy_some_harvesting`).

- [ ] **Step 3: Fabricate `gddr_telemetry`**

This is `generate_smbus_telemetry(&self, device_idx: usize)` (around line
286-419). It already computes `is_harvested` (line 297: `self.scenario ==
MockScenario::QuadGalaxy && device_idx % 17 == 0`, currently only used for
the Tensix-column `enabled_tensix_col` mask) and a sinusoidal temp base
(`t_base`/`t_var`, lines 305-306) reused across all four `gddr_temps` pairs.
`gddr_telemetry` always models exactly **8 channels** regardless of
`num_channels` (`device.memory_channels()`, line 288, which is
architecture-specific — 12 for Blackhole — a different count for a
different purpose; see the plan's Global Constraints).

Add, right before the `SmbusTelemetry { ... }` literal (after
`harvesting_state` is computed, line 311-315):

```rust
        // Per-channel GDDR telemetry (tt-smi 6.3.0+ shape). Reuses t_base/t_var
        // so temps stay consistent with the legacy gddr_temps fabrication
        // above; QuadGalaxy additionally harvests channel 3 and fails BIST on
        // channel 5 (on the same device that harvests a Tensix column) so
        // both new visual states are reachable in a demo/test run.
        let gddr_channels: Vec<crate::models::telemetry::GddrChannel> = (0..8)
            .map(|ch| {
                let temp = t_base + t_var + ch as f32 * 0.5;
                let is_harvested_channel = is_harvested && ch == 3;
                let is_bist_failed_channel = is_harvested && ch == 5;
                crate::models::telemetry::GddrChannel {
                    channel: ch,
                    harvested: is_harvested_channel,
                    enabled: !is_harvested_channel,
                    training_pass: !is_harvested_channel,
                    bist_pass: !is_bist_failed_channel,
                    temp_top: Some(temp),
                    temp_bottom: Some(temp + 2.0),
                    corr_rd: 0,
                    corr_wr: 0,
                    uncorr_rd: 0,
                    uncorr_wr: 0,
                }
            })
            .collect();
        let gddr_telemetry = Some(crate::models::telemetry::GddrTelemetry {
            speed: Some("16G".to_string()),
            max_temp: Some(t_base + t_var + 6.0),
            enabled_mask: Some(0xff),
            channels: gddr_channels,
        });
```

Then add `gddr_telemetry,` as a new field in the `SmbusTelemetry { ... }`
literal at the end of this function (after `enabled_tensix_col,` on line
417) — this literal has no `..Default::default()`, so Task 1 will already
have added a placeholder `gddr_telemetry: None,` here to keep the crate
compiling; replace that placeholder with `gddr_telemetry,` (using the
shorthand, since the local variable is already named `gddr_telemetry`).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib quad_galaxy_scenario_produces_harvested_and_bist_failed_gddr_channels`
Expected: PASS.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test --lib --features tui`
Expected: all pass, including the existing `any_harvested` Tensix-column
test (unaffected — this task only adds GDDR fabrication alongside it).

- [ ] **Step 6: Commit**

```bash
git add src/backend/mock.rs
git commit -m "feat(mock): fabricate gddr_telemetry with a harvested + BIST-fail scenario"
```

---

### Task 5: `chip_portrait.rs` — real per-channel training/harvest/BIST/dual-temp

**Files:**
- Modify: `src/ui/tui/chip_portrait.rs`

**Interfaces:**
- Consumes: `SmbusTelemetry.gddr_telemetry` (Tasks 1-2).

**Context for the implementer:** This file already renders one DRAM cell per
chip-grid column as `'▪'` (in `build_portrait_rows`, the `CoreType::Dram =>
'▪'` arm) and colors it via `dram_color_for_col` in `build_portrait_lines`
(a closure that maps a chip column to a packed `gddr_temps` pair index via
`(col / 4).min(3)` and takes the max of that pair's 4 temps). It also already
has a **column → DRAM-channel-index** mapping used by `trained_random_col`:
counting how many earlier columns are also DRAM columns
(`(0..c).filter(|&cc| is_dram_col(cc)).count()`), capped at 8 channels (BH
has 12 physical DRAM columns but tt-smi/`DDR_STATUS` only ever reports 8 —
`gddr_telemetry.channels` is the same 8, never derive the channel count from
`Architecture::memory_channels()`).

- [ ] **Step 1: Write the failing tests**

Add to `chip_portrait.rs`'s test module, alongside the existing
`test_harvested_col_shows_dot` test:

```rust
#[test]
fn dram_col_uses_real_gddr_telemetry_harvested_state_when_present() {
    use crate::models::telemetry::{GddrChannel, GddrTelemetry};
    let mut smbus = SmbusTelemetry::default();
    smbus.gddr_telemetry = Some(GddrTelemetry {
        speed: None,
        max_temp: None,
        enabled_mask: None,
        channels: vec![GddrChannel {
            channel: 0,
            harvested: true,
            enabled: false,
            training_pass: false,
            bist_pass: true,
            temp_top: None,
            temp_bottom: None,
            corr_rd: 0,
            corr_wr: 0,
            uncorr_rd: 0,
            uncorr_wr: 0,
        }],
    });
    // (fill in whichever helper/args this test file's existing
    // test_harvested_col_shows_dot uses to build a full-row render and
    // locate the first DRAM cell for Blackhole channel 0 — mirror its setup
    // exactly, swapping in the new `smbus` value above instead of the
    // Tensix-harvesting `enabled_tensix_col` bitmask it uses.)
}

#[test]
fn dram_col_falls_back_to_packed_bitmask_when_gddr_telemetry_absent() {
    // A SmbusTelemetry with gddr_telemetry: None must render identically to
    // how this file already behaves today (regression guard — the existing
    // packed-register path must be completely untouched).
}
```

Write both tests' bodies by directly mirroring the exact render-and-assert
pattern `test_harvested_col_shows_dot` already uses in this file (read it
first) — the plan can't predict its exact helper signatures without risking
staleness against the real file; match its style precisely rather than
inventing a new one.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib dram_col_uses_real_gddr_telemetry_harvested_state_when_present dram_col_falls_back_to_packed_bitmask_when_gddr_telemetry_absent`
Expected: FAIL (new behavior not implemented yet; the fallback test may
already pass since it's a no-op today — that's fine, it becomes a real
regression guard once Step 3 lands).

- [ ] **Step 3: Implement the real-data path**

In `build_portrait_rows`'s `CoreType::Dram => '▪'` arm, change it to check
for real per-channel state first:

```rust
                CoreType::Dram => {
                    let dram_channel_idx = if device.architecture == Architecture::Blackhole {
                        // Same "count earlier DRAM columns" mapping trained_random_col
                        // already uses — extract it to a shared helper if convenient,
                        // but do not change trained_random_col's own behavior.
                        let is_dram_col = |c: usize| c != 8 && core_type_bh(c, 0) == CoreType::Dram;
                        if is_dram_col(col) {
                            Some((0..col).filter(|&cc| is_dram_col(cc)).count())
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    let real_channel = dram_channel_idx.and_then(|idx| {
                        smbus
                            .and_then(|s| s.gddr_telemetry.as_ref())
                            .and_then(|g| g.channels.get(idx))
                    });
                    match real_channel {
                        Some(c) if c.harvested => '·',
                        Some(c) if !c.bist_pass => '✗',
                        _ => '▪',
                    }
                }
```

In `build_portrait_lines`'s style match, add corresponding color arms
**before** the existing `(CoreType::Dram, _) => Style::default().fg(dram_color_for_col(col))`
arm (Rust matches in order, so more specific patterns on `ch` must come
first):

```rust
                    // DRAM harvested ('·'): dim gray, no heatmap — mirrors
                    // the existing Tensix-harvested treatment.
                    (CoreType::Dram, '·') => Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                    // DRAM BIST-fail ('✗'): alarm red — a real fault, not a
                    // temperature reading.
                    (CoreType::Dram, '✗') => Style::default()
                        .fg(Color::Rgb(255, 90, 90))
                        .add_modifier(Modifier::BOLD),
                    // DRAM: blue→amber→red by per-column GDDR temperature
                    (CoreType::Dram, _) => Style::default().fg(dram_color_for_col(col)),
```

Then update `dram_color_for_col` to prefer real dual-sensor temps when
available, falling back to the packed-pair decode otherwise:

```rust
    let dram_color_for_col = |col: usize| -> Color {
        let real_channel_temp = if device.architecture == Architecture::Blackhole {
            let is_dram_col = |c: usize| c != 8 && core_type_bh(c, 0) == CoreType::Dram;
            if is_dram_col(col) {
                let idx = (0..col).filter(|&cc| is_dram_col(cc)).count();
                smbus
                    .and_then(|s| s.gddr_telemetry.as_ref())
                    .and_then(|g| g.channels.get(idx))
                    .and_then(|c| match (c.temp_top, c.temp_bottom) {
                        (Some(t), Some(b)) => Some(t.max(b)),
                        (Some(t), None) | (None, Some(t)) => Some(t),
                        (None, None) => None,
                    })
            } else {
                None
            }
        } else {
            None
        };
        let temp = real_channel_temp.unwrap_or_else(|| {
            let pair_idx = (col / 4).min(3);
            smbus
                .and_then(|s| s.gddr_temps.get(pair_idx).and_then(|p| p.as_ref()))
                .map(|p| p.0.iter().copied().fold(f32::NEG_INFINITY, f32::max))
                .unwrap_or(0.0)
        });
        let [r, g, b] = dram_temp_rgb(temp);
        Color::Rgb(r, g, b)
    };
```

(The `is_dram_col`/channel-index computation is now duplicated three times
in this file — in `trained_random_col`, the glyph-selection arm above, and
this closure. Extracting a shared `fn bh_dram_channel_index(col: usize) ->
Option<usize>` helper is encouraged if it's a clean, small change, but not
required — don't let a refactor balloon this task; duplication is
acceptable here if extraction proves awkward given the file's existing
structure.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib dram_col_uses_real_gddr_telemetry_harvested_state_when_present dram_col_falls_back_to_packed_bitmask_when_gddr_telemetry_absent test_harvested_col_shows_dot`
Expected: all PASS — including the pre-existing Tensix-harvesting test,
proving this change didn't disturb it.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test --lib --features tui`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add src/ui/tui/chip_portrait.rs
git commit -m "feat(chip_portrait): render real per-channel GDDR harvest/BIST/temp state"
```

---

### Task 6: `memory_flow.rs` — real per-channel state via the existing status vocabulary

**Files:**
- Modify: `src/animation/memory_flow.rs`

**Interfaces:**
- Consumes: `SmbusTelemetry.gddr_telemetry` (Tasks 1-2).

**Context:** `parse_ddr_status` decodes the packed bitmask into a `Vec<u8>`
per channel: `0`=idle/untrained, `1`=training, `2`=trained, anything else
(`>2`)=error (already rendered in bright red by both
`render_ddr_channels_top_agg`/`bottom_agg`'s `match status { 2 => ..., 1 =>
..., _ => ... }`, where the `_` arm's `if status > 2` check is the existing
error path). This task adds a second status source that maps real
`gddr_telemetry` state onto the **same** vocabulary — so the rendering
`match` blocks in both functions need **zero changes**; only the function
that produces the `Vec<u8>` changes.

- [ ] **Step 1: Write the failing tests**

Add to `memory_flow.rs`'s test module:

```rust
#[test]
fn channel_status_prefers_real_gddr_telemetry_over_bitmask() {
    use crate::models::telemetry::{GddrChannel, GddrTelemetry, SmbusTelemetry};
    let smbus = SmbusTelemetry {
        // Bitmask says "all trained" (nibble 2 repeated) — if this were used,
        // every channel would show status 2. gddr_telemetry disagrees, and
        // must win.
        ddr_status: Some("0x22222222".to_string()),
        gddr_telemetry: Some(GddrTelemetry {
            speed: None,
            max_temp: None,
            enabled_mask: None,
            channels: vec![
                GddrChannel { channel: 0, harvested: true, enabled: false, training_pass: false, bist_pass: true, ..Default::default() },
                GddrChannel { channel: 1, harvested: false, enabled: true, training_pass: true, bist_pass: false, ..Default::default() },
                GddrChannel { channel: 2, harvested: false, enabled: true, training_pass: true, bist_pass: true, ..Default::default() },
            ],
        }),
        ..Default::default()
    };
    let status = MemoryFlowVis::channel_status(Some(&smbus), 3);
    assert_eq!(status[0], 0, "harvested channel reads as idle, not trained");
    assert_eq!(status[1], 3, "BIST failure is a distinct error code, not the generic bitmask error");
    assert_eq!(status[2], 2, "healthy enabled channel reads as trained");
}

#[test]
fn channel_status_falls_back_to_bitmask_when_gddr_telemetry_absent() {
    use crate::models::telemetry::SmbusTelemetry;
    let smbus = SmbusTelemetry {
        ddr_status: Some("0x00000012".to_string()), // channel0=trained(2), channel1=idle(0), channel... etc per nibble
        gddr_telemetry: None,
        ..Default::default()
    };
    let status = MemoryFlowVis::channel_status(Some(&smbus), 2);
    assert_eq!(status[0], 2);
    assert_eq!(status[1], 1);
}
```

(Adjust the exact struct/function path — `MemoryFlowVis::channel_status` — to
match whatever `parse_ddr_status`'s actual `impl` block and `Self` type are
in this file; it's a method on the same type per the existing
`Self::parse_ddr_status(smbus, num_channels)` call sites.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib channel_status_prefers_real_gddr_telemetry_over_bitmask channel_status_falls_back_to_bitmask_when_gddr_telemetry_absent`
Expected: FAIL to compile — `channel_status` doesn't exist yet.

- [ ] **Step 3: Add `channel_status` and route both render functions through it**

Add, near `parse_ddr_status` (leave `parse_ddr_status` itself completely
unchanged — it's still used as the fallback):

```rust
    /// Map real per-channel `gddr_telemetry` state onto the same status
    /// vocabulary `parse_ddr_status` produces from the packed bitmask
    /// (0=idle/untrained, 1=training, 2=trained, >2=error), so the existing
    /// render match in `render_ddr_channels_top_agg`/`bottom_agg` needs no
    /// changes. `gddr_telemetry` has no "currently training" signal (only a
    /// post-hoc pass/fail), so this source never emits `1` — that state
    /// stays reachable only via the bitmask fallback.
    fn channel_status_from_gddr_telemetry(
        g: &crate::models::telemetry::GddrTelemetry,
        num_channels: usize,
    ) -> Vec<u8> {
        (0..num_channels)
            .map(|i| {
                g.channels.get(i).map_or(0, |c| {
                    if c.harvested || !c.enabled {
                        0
                    } else if !c.training_pass || !c.bist_pass {
                        3
                    } else {
                        2
                    }
                })
            })
            .collect()
    }

    /// Real `gddr_telemetry` state when present (tt-smi ≥ 6.3.0), else the
    /// packed `DDR_STATUS` bitmask decode (`parse_ddr_status`).
    fn channel_status(
        smbus: Option<&crate::models::SmbusTelemetry>,
        num_channels: usize,
    ) -> Vec<u8> {
        match smbus
            .and_then(|s| s.gddr_telemetry.as_ref())
            .filter(|g| !g.channels.is_empty())
        {
            Some(g) => Self::channel_status_from_gddr_telemetry(g, num_channels),
            None => Self::parse_ddr_status(smbus, num_channels),
        }
    }
```

Then in both `render_ddr_channels_top_agg` and `render_ddr_channels_bottom_agg`,
replace:

```rust
        let channel_status = Self::parse_ddr_status(smbus, num_channels);
```

with:

```rust
        let channel_status = Self::channel_status(smbus, num_channels);
```

(Two call sites — one in each function.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib channel_status_prefers_real_gddr_telemetry_over_bitmask channel_status_falls_back_to_bitmask_when_gddr_telemetry_absent`
Expected: both PASS.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test --lib --features tui`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add src/animation/memory_flow.rs
git commit -m "feat(memory_flow): drive DDR channel rendering from real gddr_telemetry when present"
```

---

### Task 7: `memory_castle.rs` — DDR gate row driven by real per-channel state

**Files:**
- Modify: `src/animation/memory_castle.rs`

**Interfaces:**
- Consumes: `SmbusTelemetry.gddr_telemetry` (Tasks 1-2).

**Context:** The DDR gate row (around line 973-1006) currently renders
`((channels + 1) / 2).min(col_width)` glyphs — one per **pair** of channels
— all `'▪'`, colored uniformly by a hue-cycling formula
(`210.0 + frame*0.9 + idx*12.0`) modulated only by generic `current_change`
activity. This task changes it to render one glyph **per real channel**
(not per pair) with per-channel color, when `gddr_telemetry` is present;
falls back to the existing per-pair/generic-activity rendering otherwise.

- [ ] **Step 1: Write the failing test**

Add to `memory_castle.rs`'s test module:

```rust
#[test]
fn ddr_gate_row_uses_real_channel_count_and_state_when_present() {
    // Construct a MockBackend (or whatever this file's existing tests use
    // to drive `render`) with one device whose SmbusTelemetry has a
    // gddr_telemetry block with a distinctive channel count (e.g. 3
    // channels, one harvested) that differs from
    // `device.memory_channels() / 2` gate-pair count, and assert the
    // rendered gate row reflects 3 real channels, not the generic pair
    // count. Mirror this file's existing test setup pattern exactly (find
    // an existing `render`-driving test in this file's test module and
    // copy its harness) rather than inventing a new one — the plan cannot
    // specify exact backend-construction calls without risking staleness.
}
```

Write the test body by directly copying the setup of an existing test in
this file that already calls `render()` on a constructed backend/device, and
adapt only the assertion.

- [ ] **Step 2: Run test to verify it fails**

Run the new test by name; expected: FAIL (new behavior not implemented, or a
compile error if the test's mock-construction guess needs fixing to match
the file's real API — fix the test to compile first, then confirm the
assertion fails against current behavior).

- [ ] **Step 3: Implement the real-data path**

Replace the DDR gate row's per-device closure body (the block computing
`channels`, `gates`, `hue`, `glow`, `color`, `glyphs` around lines 980-1000)
with a branch: when `backend.smbus_telemetry(device.index)`'s
`gddr_telemetry` is present with a non-empty `channels` list, build one span
per real channel (glyph + color chosen per-channel: harvested → dim gray
`'·'`, BIST-fail → alarm red `'✗'`, else the existing hue-cycling `'▪'`
formula) instead of the uniform per-pair glyph string; otherwise keep the
current behavior byte-for-byte. Since different channels now need different
colors within the same "cell" (previously one `Span` per device holding a
uniform-colored repeated-glyph string), build a small `Vec<Span>` of
single-character spans for the real-channel case instead of one merged
string, e.g.:

```rust
                let smbus = backend.smbus_telemetry(device.index);
                let real_channels = smbus
                    .and_then(|s| s.gddr_telemetry.as_ref())
                    .filter(|g| !g.channels.is_empty());

                let glyph_spans: Vec<Span> = if let Some(g) = real_channels {
                    let n = g.channels.len().min(col_width).max(1);
                    g.channels
                        .iter()
                        .take(n)
                        .map(|c| {
                            let (ch, color) = if c.harvested {
                                ('·', Color::DarkGray)
                            } else if !c.bist_pass {
                                ('✗', Color::Rgb(255, 90, 90))
                            } else {
                                let hue = (210.0 + self.frame as f32 * 0.9 + idx as f32 * 12.0) % 360.0;
                                let glow = (0.45 + current_change.clamp(0.0, 1.0) * 0.45).min(1.0);
                                ('▪', hsv_to_rgb(hue, 0.85, glow))
                            };
                            Span::styled(ch.to_string(), Style::default().bg(colors::rgb(0, 0, 0)).fg(color))
                        })
                        .collect()
                } else {
                    let gates = ((channels + 1) / 2).min(col_width);
                    let hue = (210.0 + self.frame as f32 * 0.9 + idx as f32 * 12.0) % 360.0;
                    let glow = (0.45 + current_change.clamp(0.0, 1.0) * 0.45).min(1.0);
                    let color = hsv_to_rgb(hue, 0.85, glow);
                    vec![Span::styled(
                        "▪".repeat(gates),
                        Style::default().bg(colors::rgb(0, 0, 0)).fg(color),
                    )]
                };
                let glyph_count: usize = glyph_spans.iter().map(|s| s.content.chars().count()).sum();
                let padding_needed = col_width.saturating_sub(glyph_count);
                let left_pad = " ".repeat(padding_needed / 2);
                let right_pad = " ".repeat(padding_needed - padding_needed / 2);

                let mut spans = vec![Span::styled(left_pad, Style::default())];
                spans.extend(glyph_spans);
                spans.push(Span::styled(right_pad, Style::default()));
                spans
```

(This replaces the closure body that previously ended in the 3-element
`vec![left_pad, glyphs_span, right_pad]` — the padding/left/right structure
is preserved, only the middle glyph-building part changes shape.)

- [ ] **Step 4: Run test to verify it passes**

Run the new test by name; expected: PASS.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test --lib --features tui`
Expected: all pass, including any existing test asserting the old uniform
gate-row rendering for the no-`gddr_telemetry` case (must still pass
unchanged — that's the fallback path).

- [ ] **Step 6: Commit**

```bash
git add src/animation/memory_castle.rs
git commit -m "feat(memory_castle): drive DDR gate row from real per-channel gddr_telemetry"
```

---

### Task 8: `starfield.rs` — DDR planets driven by real per-channel temp/harvest/BIST

**Files:**
- Modify: `src/animation/starfield.rs`

**Interfaces:**
- Consumes: `SmbusTelemetry.gddr_telemetry` (Tasks 1-2).

**Context:** DDR planets (`planet.level == 2`) already carry a
`channel_idx: usize` field set at spawn time (one planet per channel, per
`device.memory_channels()`). The per-tick update loop (in the function
containing the `match planet.level { 0 => ..., 1 => ..., 2 => ..., _ => 0.0
}` activity computation, and the parallel `planet_hue` match right after)
currently drives DDR planets from a generic `(power_change + current_change)
/ 2.0` formula and a uniform hue-cycling formula offset by `channel_idx`.
This task adds a real-data branch for `level == 2` only — L1 (`level == 0`)
and L2 (`level == 1`) planets are untouched.

- [ ] **Step 1: Write the failing test**

Add to `starfield.rs`'s test module:

```rust
#[test]
fn ddr_planet_activity_reflects_real_channel_temp_when_present() {
    // Construct whatever this file's existing planet-update tests use
    // (a HardwareStarfield + mock/fake backend), set device 0's
    // SmbusTelemetry.gddr_telemetry with channel 0 at a known high temp
    // (e.g. temp_top=80, temp_bottom=80) and channel 1 harvested, run one
    // update tick, and assert: the level==2/channel_idx==0 planet's
    // activity is high (driven by the real 80°C reading, not the generic
    // power/current formula) and the level==2/channel_idx==1 planet's
    // activity is 0.0 (harvested). Mirror this file's existing test setup
    // pattern for constructing a backend and calling `update()` — copy an
    // existing test's harness rather than inventing one.
}
```

- [ ] **Step 2: Run test to verify it fails**

Run the new test by name; expected: FAIL (compile error if the harness guess
needs fixing — fix to compile, then confirm the assertion fails against
current behavior).

- [ ] **Step 3: Implement the real-data path**

In the `planet.activity = match planet.level { ... }` block, change the
`2 => { ... }` arm:

```rust
                    2 => {
                        // DDR: prefer real per-channel gddr_telemetry state.
                        let real_channel = backend
                            .smbus_telemetry(planet.device_idx)
                            .and_then(|s| s.gddr_telemetry.as_ref())
                            .and_then(|g| g.channels.get(planet.channel_idx));
                        match real_channel {
                            Some(c) if c.harvested => 0.0,
                            Some(c) if !c.bist_pass => 1.0, // max activity: flag the fault
                            Some(c) => {
                                let avg_temp = match (c.temp_top, c.temp_bottom) {
                                    (Some(t), Some(b)) => (t + b) / 2.0,
                                    (Some(t), None) | (None, Some(t)) => t,
                                    (None, None) => 0.0,
                                };
                                (avg_temp / 90.0).clamp(0.0, 1.0)
                            }
                            None => {
                                let power = telem.power_w();
                                let power_change = self.baseline.power_change(planet.device_idx, power);
                                ((power_change + current_change) / 2.0).max(0.0).min(1.0)
                            }
                        }
                    }
```

And the parallel `planet_hue` match's `_ => (frame*2.5 + channel_idx*30) %
360` arm (the fallthrough arm that currently covers `level == 2`), change to
check the same `real_channel` lookup — harvested → skip hue entirely (set
color directly to a dim gray after the match, or special-case it in the
match itself), BIST-fail → alternate between a bright red and dark value
based on `self.frame` parity for a flicker effect, else keep the existing
hue-cycling formula:

```rust
                let real_ddr_channel = (planet.level == 2)
                    .then(|| {
                        backend
                            .smbus_telemetry(planet.device_idx)
                            .and_then(|s| s.gddr_telemetry.as_ref())
                            .and_then(|g| g.channels.get(planet.channel_idx))
                    })
                    .flatten();
                if let Some(c) = real_ddr_channel {
                    if c.harvested {
                        planet.color = Color::Rgb(60, 60, 60);
                    } else if !c.bist_pass {
                        let flicker = (self.frame / 3) % 2 == 0;
                        planet.color = if flicker {
                            Color::Rgb(255, 60, 60)
                        } else {
                            Color::Rgb(90, 20, 20)
                        };
                    } else {
                        let planet_hue = (self.frame as f32 * 2.5 + planet.channel_idx as f32 * 30.0) % 360.0;
                        let planet_value = 0.6 + planet.activity * 0.4;
                        planet.color = _hsv(planet_hue, 1.0, planet_value);
                    }
                } else {
                    // existing planet_hue match + hsv_to_rgb call, unchanged, for
                    // level 0/1 and the no-real-data DDR fallback.
                }
```

(Fit this into the existing match/color-assignment structure — the plan is
giving you the decision logic, not a verbatim drop-in replacement, since the
exact surrounding match arms for level 0/1 must stay byte-for-byte as they
are today. Read the ~40 lines around the existing `planet_hue` match before
editing so the L1/L2 arms are provably untouched.)

- [ ] **Step 4: Run test to verify it passes**

Run the new test by name; expected: PASS.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test --lib --features tui`
Expected: all pass, including any existing starfield planet-activity tests
for L1/L2 (must be completely unaffected) and the DDR no-real-data fallback.

- [ ] **Step 6: Commit**

```bash
git add src/animation/starfield.rs
git commit -m "feat(starfield): drive DDR planets from real per-channel gddr_telemetry"
```

---

### Task 9: Insights sidebar — upgraded GDDR temp row + new summary row

**Files:**
- Modify: `src/ui/tui/mod.rs`

**Interfaces:**
- Consumes: `SmbusTelemetry.gddr_telemetry` (Tasks 1-2).

- [ ] **Step 1: Write the failing test**

Add a test near this file's existing Insights-sidebar-row tests (search for
tests exercising the GDDR ECC row or GDDR T row rendering, and mirror their
harness):

```rust
#[test]
fn gddr_summary_row_shows_trained_harvested_bist_fail_counts_when_present() {
    // Construct a SmbusTelemetry with gddr_telemetry: 3 channels, one
    // harvested, one BIST-failed, one healthy-and-trained. Call whatever
    // function builds the Insights sidebar's stat_lines (find it — likely
    // the same function containing the existing "GDDR T" and "ECC" row
    // construction shown in the plan's design research) and assert a new
    // row's text contains "1/3 trained" (or whatever exact wording you
    // choose — pick something and keep it consistent with this file's
    // existing terse row style like "8/? live" for ETH) plus mentions of
    // "harvested" and "BIST" counts.
}
```

- [ ] **Step 2: Run test to verify it fails**

Run the new test by name; expected: FAIL (row doesn't exist yet, or compile
error if the harness guess needs adjusting to match the real function
signature — fix to compile first).

- [ ] **Step 3: Upgrade the GDDR temp row and add the summary row**

Find the "GDDR temp row (full mode only" block (currently reads
`s.gddr_temps`, computes `t_min`/`t_max` across all packed-pair values).
Change its temp collection to prefer real per-channel `temp_top`/
`temp_bottom` values when `gddr_telemetry` is present (skip harvested
channels — a harvested channel's temp is meaningless), falling back to the
existing packed-pair collection otherwise:

```rust
            let temps: Vec<f32> = s
                .gddr_telemetry
                .as_ref()
                .filter(|g| !g.channels.is_empty())
                .map(|g| {
                    g.channels
                        .iter()
                        .filter(|c| !c.harvested)
                        .flat_map(|c| [c.temp_top, c.temp_bottom])
                        .flatten()
                        .filter(|&t| t > 0.0)
                        .collect::<Vec<f32>>()
                })
                .unwrap_or_else(|| {
                    s.gddr_temps
                        .iter()
                        .filter_map(|p| p.as_ref())
                        .flat_map(|p| p.0.iter().copied())
                        .filter(|&t| t > 0.0)
                        .collect()
                });
```

(This replaces just the `let temps: Vec<f32> = ...` computation; the
`t_min`/`t_max`/color/row-push code immediately after is unchanged.)

Then, immediately after that row's closing (still inside the `if !compact {
if let Some(s) = smbus { ... } }` block), add the new summary row:

```rust
            if let Some(g) = s.gddr_telemetry.as_ref().filter(|g| !g.channels.is_empty()) {
                let total = g.channels.len();
                let harvested = g.channels.iter().filter(|c| c.harvested).count();
                let trained = g
                    .channels
                    .iter()
                    .filter(|c| !c.harvested && c.training_pass)
                    .count();
                let bist_failed = g
                    .channels
                    .iter()
                    .filter(|c| !c.harvested && !c.bist_pass)
                    .count();
                let color = if bist_failed > 0 {
                    Color::Rgb(255, 80, 80)
                } else if harvested > 0 {
                    Color::Rgb(244, 196, 113)
                } else {
                    Color::Rgb(79, 209, 197)
                };
                stat_lines.push(Line::from(vec![
                    Span::styled(
                        format!("{:<8}", "GDDR"),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!(
                            "{}/{} trained · {} harvested · {} BIST-fail",
                            trained, total, harvested, bist_failed
                        ),
                        Style::default().fg(color),
                    ),
                ]));
            }
```

- [ ] **Step 4: Run test to verify it passes**

Run the new test by name; expected: PASS.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test --lib --features tui`
Expected: all pass, including any existing GDDR-T-row / GDDR-ECC-row tests
(the ECC row itself is untouched by this task).

- [ ] **Step 6: Commit**

```bash
git add src/ui/tui/mod.rs
git commit -m "feat(insights): upgrade GDDR temp row and add trained/harvested/BIST-fail summary"
```

---

## Manual verification (not automated — no TT hardware with tt-smi 6.3.0 in CI)

1. Install tt-smi 6.3.0 (`pip install tt-smi==6.3.0` in a venv, or system
   package once available) on a box with real Blackhole hardware.
2. Run `tt-toplike-tui --backend json` and confirm: `chip_portrait` shows
   real per-channel DRAM cell colors/glyphs (harvested channels dim, any
   BIST-failed channel shows `✗` in red); the Insights sidebar's new GDDR row
   shows real trained/harvested/BIST-fail counts; `memory_flow`/
   `memory_castle`/`starfield` visibly reflect real per-channel state
   (compare against `--backend sysfs` on the same box, which won't have
   `gddr_telemetry` — the fallback rendering should look like today's
   behavior there).
3. Note the result (pass/fail, tt-smi/tt-kmd versions, what was checked) in
   `AGENTS.md`'s dev log, consistent with how every other telemetry change
   records its hardware-verification status.
