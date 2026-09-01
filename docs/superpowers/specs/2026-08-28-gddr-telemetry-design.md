# tt-smi 6.3.0 `gddr_telemetry` → memory visualizations (design)

**Status:** approved in chat 2026-08-28.
**Date:** 2026-08-28
**Extends:** the JSON backend's tt-smi parsing (`src/backend/json.rs`), the
`SmbusTelemetry` model, and every memory-hierarchy visualization
(`chip_portrait.rs`, `memory_flow.rs`, `memory_castle.rs`, `starfield.rs`,
`arcade.rs`) plus the Insights sidebar.

## Problem

tt-smi 6.3.0 (installed here: 6.1.0; latest is 6.3.0) adds a new top-level
`gddr_telemetry` block to each `device_info[]` entry — confirmed absent in
6.2.0, present in 6.3.0, verified live against a real 4× Blackhole snapshot.
It carries **already-decoded, per-channel** GDDR state that today's
`SmbusTelemetry` only approximates from packed hex registers:

```json
"gddr_telemetry": {
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
    ...
  ]
}
```

Every field except `channel` (int) and `harvested`/`enabled` (bool) is a
**string-encoded number** — the same "tt-smi emits quoted numbers"
inconsistency `DeviceLimitsRaw` already works around.

Not currently parsed anywhere in tt-toplike. This is a strict upgrade over
what's parsed today:

| Today (`SmbusTelemetry`) | New (`gddr_telemetry`) |
|---|---|
| `ddr_status` packed 4-bit nibble per channel, `is_ddr_channel_trained()` | `training`: `"pass"`/other, per channel, no bit-unpacking |
| *(nothing)* | `bist`: `"pass"`/other — **new capability**, no current equivalent |
| *(nothing — only Tensix-column harvesting exists)* | `harvested`: bool per **GDDR** channel — new |
| `gddr_temps: [Option<GddrTempPair>; 4]`, 2 channels packed per `u32` register | `temp_top`/`temp_bottom`: two real sensors per channel |
| `gddr_corr_errs`/`gddr_uncorr_errs`: aggregate per register-pair | `corr_rd`/`corr_wr`/`uncorr_rd`/`uncorr_wr`: split by direction, per channel |

## Grounded facts (live snapshot, 2026-08-27/28)

- 8 channels on Blackhole (`channels` array length), **not** 12 — `Device`'s
  existing `Architecture::memory_channels()` returns 12 for Blackhole (real
  physical DRAM channels); `gddr_telemetry` only ever reports the 8 tt-smi
  addresses, same count `chip_portrait.rs` already caps at for the exact same
  reason (`ddr_status` also only encodes 8). **Never derive the loop bound
  from `memory_channels()` for this data — use `channels.len()`.**
- Device-level rollup fields: `speed` (e.g. `"16G"`), `max_temp` (already the
  max across channels — tt-smi computes it, don't recompute), `enabled_mask`
  (hex string, same shape as the existing `enabled_gddr` field).
- `gddr_telemetry` is a **device-level sibling** of `board_info`/`telemetry`/
  `smbus_telem`/`firmwares`/`limits` in the raw JSON — structurally identical
  in *location* to `firmwares`/`limits` (which is why they're the parsing
  precedent to copy), but **semantically dynamic** (temps/ECC/training state
  genuinely change tick to tick), which is why it's modeled and *stored*
  differently — see Design §1.

## Design

### 1. Where it lives: `SmbusTelemetry`, not `Device`

`firmwares`/`limits` live on `Device` because they're static device identity
— parsed once, never expire, never re-checked. `gddr_telemetry`'s fields are
measurements: temps drift, ECC counters accrue, and (in principle) training
could be re-run. Storing it on `Device` would mean it never expires even if
the tt-smi reader dies — exactly the staleness bug Phase 26 fixed for the
rest of the SMBUS surface. Storing it on `SmbusTelemetry` means it inherits,
for free:

- The hybrid backend's existing 10s whole-SMBUS-surface staleness expiry
  (Phase 26) — no new expiry logic needed.
- `smbus_smooth.rs`'s `blend()` — but that function enumerates every
  `SmbusTelemetry` field **by hand**, and Phase 25's own postmortem is "adding
  a field to `SmbusTelemetry` means adding a line here too — the compiler
  will not tell you." This bit real bugs twice already. See §5 for how this
  design makes that mistake loud instead of silent.

New model in `models/telemetry.rs`:

```rust
/// One GDDR channel's state, from tt-smi ≥ 6.3.0's `gddr_telemetry.channels[]`.
/// Every numeric field arrives as a quoted string in the real JSON (the same
/// inconsistency `DeviceLimitsRaw` already works around) — parsed tolerantly
/// in `json.rs`, never via a derived `Deserialize` on this struct.
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
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GddrTelemetry {
    pub speed: Option<String>,
    pub max_temp: Option<f32>,
    pub enabled_mask: Option<u32>,
    pub channels: Vec<GddrChannel>,
}
```

Add `pub gddr_telemetry: Option<GddrTelemetry>` to `SmbusTelemetry`
(alongside the existing packed fields — those stay, unchanged, as the
fallback for tt-smi < 6.3.0 and for backends that never produce the new
block).

### 2. Parsing (`json.rs`)

Structural precedent: `firmwares`/`limits` (raw `serde_json::Value`
passthrough on `TTSMIDeviceRaw` **and** `TTSMIDeviceJSON`, decoded via
`serde_json::from_value` at the consuming site, `None` on decode failure —
never fails the whole device). Add:

```rust
// on both TTSMIDeviceRaw and TTSMIDeviceJSON, alongside firmwares/limits:
pub gddr_telemetry: Option<serde_json::Value>,
```

wired through the same two closures that already copy `firmwares`/`limits`
(`parsed_from_json_devices`'s map, and `salvage_modern_snapshot`'s per-entry
closure).

**Decoding is NOT a derived `Deserialize`** on `GddrTelemetry`/`GddrChannel`
directly — the string-encoded numbers would fail a strict derive the same
way `DeviceLimitsRaw` had to route around. Instead, a manual tolerant parser
in `json.rs`:

```rust
/// Parse tt-smi ≥ 6.3.0's `gddr_telemetry` block. Every numeric leaf is a
/// quoted string in the real JSON except `channel` (already a number) —
/// tolerant of both, via the existing `parse_hex_or_dec`-style helpers.
/// A channel entry that fails to parse a required field is skipped (not
/// fabricated as a zeroed channel), matching this backend's "one malformed
/// entry costs only that entry" convention (see `salvage_modern_snapshot`).
fn parse_gddr_telemetry(v: &serde_json::Value) -> Option<GddrTelemetry> {
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
    Some(GddrTelemetry { speed, max_temp, enabled_mask, channels })
}

fn parse_gddr_channel(v: &serde_json::Value) -> Option<GddrChannel> {
    let obj = v.as_object()?;
    let str_f32 = |k: &str| obj.get(k)?.as_str()?.parse::<f32>().ok();
    let str_u64 = |k: &str| obj.get(k)?.as_str()?.parse::<u64>().ok();
    let str_bool_pass = |k: &str| obj.get(k)?.as_str().map(|s| s == "pass");
    Some(GddrChannel {
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

Wired in at the one place `device_from_json`/`smbus_from_json_fields` already
combine per-device pieces (`parsed_from_json_devices`, and the hybrid
backend's equivalent path around line 1077 — both need the same change):

```rust
let gddr_telemetry = json_dev
    .gddr_telemetry
    .as_ref()
    .and_then(parse_gddr_telemetry);
match json_dev.smbus {
    Some(smbus_json) => {
        let mut s = smbus_from_json_fields(smbus_json);
        s.gddr_telemetry = gddr_telemetry;
        smbus.insert(idx, s);
    }
    None if gddr_telemetry.is_some() => {
        // Phase 26 precedent: a payload that parses independently of
        // smbus_telem's presence must not be dropped just because that
        // sibling block is missing (permission-restricted run, ARC that
        // didn't answer).
        smbus.insert(idx, SmbusTelemetry { gddr_telemetry, ..Default::default() });
    }
    None => {}
}
```

### 3. `smbus_smooth.rs` — copy through, never smooth, and make forgetting loud

Per the file's own established rule (temps are a max not an average, ECC
counters are monotonic, bitmasks are meaningless interpolated): `blend()`
copies `gddr_telemetry` straight through, unconditionally (`incoming.clone()`
wins, matching how `enabled_gddr`/`harvesting_state`/etc. are already
handled — **not** merged field-by-field into the previous value).

Because "add a field, forget a line" has already caused two real bugs, this
task adds a **regression test that fails if a new `SmbusTelemetry` field
isn't touched by `blend()`** — not by reflection (this is Rust, no runtime
reflection), but by a literal-count assertion: construct a `SmbusTelemetry`
with every field `Some`/non-default via a helper that's forced to name every
field (a struct literal with no `..Default::default()`), blend it against a
`Default::default()` previous value, and assert the *entire result* equals
the incoming value. If a future field is added to the struct but not to
`blend()`, this test **fails to compile** (the no-`..Default` literal won't
compile until the new field is added there too) rather than silently passing
— turning the "compiler will not tell you" problem into one it does.

### 4. Visualization layer — real data where it existed, real data where it didn't

Every consumer below falls back to the existing packed-register-derived
behavior when `gddr_telemetry` is `None` (older tt-smi, sysfs-only, luwen,
mock without the new fabrication) — nothing regresses on hardware that
doesn't have the new data.

**`chip_portrait.rs`** (already the richest per-channel consumer):
- Per-channel training: prefer `channels[i].training_pass` over
  `is_ddr_channel_trained()` when present.
- **New: BIST-fail indicator.** A channel with `bist_pass == false` gets a
  distinct glyph/color the existing trained/training/untrained palette
  doesn't have — this is new information, not a re-skin of old information.
- **New: GDDR-channel harvested dimming.** Mirrors the existing Tensix-column
  harvested treatment (dim gray, no heatmap) — today there's no GDDR-channel
  harvest visual at all.
- Real dual-sensor temps (`temp_top`/`temp_bottom`) drive the heatmap instead
  of the packed-pair decode, when present.

**`memory_flow.rs`**:
- `render_ddr_channels_top_agg`/`bottom_agg` prefer real
  `training_pass`/`harvested`/`bist_pass` over the independent
  `parse_ddr_status()` bitmask decode when `gddr_telemetry` is present.
- New BIST-fail state, distinct from the existing training/trained/error
  states — same rationale as `chip_portrait.rs`.

**`memory_castle.rs`** (currently purely decorative — activity-colored glyphs
only, no real per-channel truth): DDR gate row driven by real per-channel
trained/harvested/BIST-fail state when present, replacing the generic
activity coloring for that row specifically. Nothing else in this file
changes.

**`starfield.rs`** (currently decorative — 8 fixed DDR "planets", synthetic
combined power/current activity): planet brightness/color driven by real
per-channel temp (`(temp_top + temp_bottom) / 2` when both present, whichever
is present otherwise) when available; harvested channels dim; a BIST-failed
channel's planet flickers. Falls back to the existing synthetic-activity
behavior when `gddr_telemetry` is absent.

**`arcade.rs`**: no direct changes — it composites `memory_castle`'s
rendering, so it inherits that file's upgrade automatically.

**Insights sidebar (`ui/tui/mod.rs`)**: new compact GDDR summary row when
`gddr_telemetry` is present — trained/harvested/BIST-failed channel counts
(e.g. `GDDR  8/8 trained · 0 harvested · 0 BIST-fail`). The existing
aggregate ECC row is unchanged and stays the fallback when the new data is
absent; when present, source its correctable/uncorrectable sums from the new
per-channel-direction data instead of the packed aggregate (strictly more
accurate — same row, better source).

### 5. Mock backend

`mock.rs` fabricates `gddr_temps` but never `gddr_corr_errs`/
`gddr_uncorr_errs`/`enabled_gddr` — no error/harvest data at all today. Add a
synthesized `gddr_telemetry` so every visualization change above is
exercisable without real tt-smi 6.3.0 hardware:
- Normal channels: `training_pass`/`bist_pass` true, `harvested`/`enabled`
  matching existing enabled-mask fabrication, temps derived from the
  existing sinusoidal `gddr_temps` generator (reuse the same wave, don't add
  a second one), zero ECC counts.
- Following the existing `MockScenario`-gated pattern (the codebase already
  varies `harvesting_state` for `QuadGalaxy`): one scenario variant harvests
  a GDDR channel and fails BIST on another, so the new visual states are
  actually reachable in a demo/test run, not just in real hardware.

## Testing

- `json.rs`: unit tests for `parse_gddr_telemetry`/`parse_gddr_channel`
  against the real snapshot shape (trimmed, like existing fixtures) —
  string-vs-bool-vs-int field handling, a malformed channel entry being
  skipped without failing the rest, and the block being entirely absent
  (older tt-smi) parsing to `None` cleanly. Also a test for the
  `smbus_telem`-absent-but-`gddr_telemetry`-present merge path.
- `smbus_smooth.rs`: the no-`..Default::default()`-literal completeness test
  from §3, plus a copy-through-unsmoothed assertion for `gddr_telemetry`
  specifically (temps/ECC counts from tick 2 must equal tick 2's raw values,
  not an average with tick 1's).
- Each visualization file: existing test conventions (e.g.
  `chip_portrait.rs` already has `test_harvested_col_shows_dot`-style tests)
  extended with a `gddr_telemetry`-present case alongside the existing
  packed-register case, proving the fallback path is untouched.
- `mock.rs`: a test that the new scenario variant actually produces a
  harvested/BIST-failed channel (regression guard, matching the existing
  `any_harvested` test for Tensix columns).

## Open items intentionally deferred

- `gddr_telemetry.speed`/`max_temp` device-level rollups aren't surfaced
  anywhere new in this pass beyond what per-channel data already implies —
  no dedicated "GDDR speed" row is added; `max_temp` isn't re-displayed
  separately since the sidebar's new summary row and the per-channel visuals
  already carry temperature information.
- No version-string gate is added anywhere (e.g. "requires tt-smi 6.3.0+" in
  a help/legend overlay) — the feature is purely presence-gated (`Option`
  chains), consistent with how every other tt-smi-version-gated field in
  this codebase already works.
