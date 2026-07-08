# Arcade duel + arcade dedup + castle compression + BBS ANSI aesthetic (design)

**Status:** approved design (duel sketch approved in session; dedup/castle/BBS
direction given by the maintainer), pending spec review.
**Date:** 2026-07-04
**Sequence:** the Arcade "completeness tribute" — the hero battles the serving
snake — plus three quality passes the deep review + maintainer surfaced:
duplicated telemetry in Arcade, the Memory Castle's premature fleet-grid
fallback, and a stronger 1990s demoscene / elite-BBS ANSI art identity.

## 1. The duel — hero ⚔ snake in Arcade (telemetry-true)

When an inference server is **serving** (any `ServiceState` Ready), the Arcade
view stages a duel between the two mascots. Every motion maps to a real signal
— nothing is scripted:

| Signal | Duel expression |
|---|---|
| chip power vs learned baseline (existing `AdaptiveBaseline`) | **hero lunges** on a spike |
| `generation_tps` + `completed_delta` | **snake lunges** on a token burst / finished request |
| tug-of-war balance: normalized hardware utilization vs normalized serving demand | a contested `⚔` marker slides along a line between them |
| `kv_cache_usage` | snake glow/heat (hot pink under grayskull) |
| thermal throttle flag (`hero_throttled`) | hero staggers (existing expression flags reused) |
| idle server (0 tps, 0 running) | snake coils at its edge and rests |
| idle chip (power ≈ baseline) | hero catches breath (slow bob) |

- **Where:** a 1-row **duel strip** replacing the Arcade's blank/topology slack
  in the header region (or directly under the title bar), spanning the width:
  hero `@` at the left, snake `●●●▶` entering from the right, the `⚔` balance
  marker between them. No new panel; costs at most 1 row.
- **Balance math (pure):** `duel_balance(hw: f32, serve: f32) -> f32` in
  `[-1, +1]` — `hw` = mean device utilization proxy (power fraction of max
  observed), `serve` = `min(1, tps/GEN_TPS_REF)` blended with queue depth.
  Marker x = center + balance × half-span. Deterministic, unit-tested.
- **Data plumbing:** Arcade's `update` today reads only the backend. It gains
  an optional `serving: Option<&ServingStats>` input threaded from the `[i]`
  monitor snapshot the TUI already holds (Linux; `None` elsewhere/off). When
  `None` or nothing Ready → no duel strip (the row stays topology/blank as
  today). Zero new probing.
- **Nobody wins permanently:** it oscillates with live load; that's the point.

## 2. Arcade dedup — one telemetry strip, not three

Confirmed duplication: the three embedded sections each print per-device
power+temp — starfield (`Dev0 92W 78°C`, starfield.rs:567), castle
(`92.0W 78.0°C`, memory_castle.rs:908), flow (`🌡 …°C ⚡ …W`, memory_flow.rs).
Same numbers three times on one composite screen.

Fix: embedded vizzes gain a **`chrome: bool`** (or `embedded: bool`) knob —
when rendered inside Arcade they suppress their own per-device telemetry
headers; Arcade renders **one** shared per-device strip (in the title/topology
header region): `Dev0 92W 78°C 1.35GHz · Dev1 85W 74°C 1.35GHz`. Standalone
modes keep their headers exactly as today (no behavior change outside Arcade).

## 3. Memory Castle — compress before falling back to fleet grid

Today `MIN_CHIP_COL_WIDTH = 15` (memory_castle.rs:493): a 120-col terminal
falls to the fleet heatmap beyond 7 devices. Add a **compact castle** tier so
the fallback ladder is: full castles → compact castles → fleet grid.

- **Compact castle column (≈8 cols min):** tower walls shrink to single-glyph
  sides, the device-info line drops to `D0 92W` (power only; temp is the
  tower's color already), DDR gate row compresses to one glyph per 2 channels,
  particle budget per device scales down. Pure layout change in
  `render()`/`render_fleet_grid()` dispatch: `full` if `width/n ≥ 15`,
  `compact` if `≥ 8`, else fleet grid. A 120-col terminal now holds 14 compact
  castles before the heatmap.
- Tested: dispatch thresholds (n devices × width → tier), compact column
  renders within its budget, no panic at boundary widths.

## 4. 1990s BBS / demoscene ANSI aesthetic pass

Lean into elite-BBS and demo-scene ANSI art across the shared chrome (the
per-view content already uses block ramps from the earlier shading pass):

- **Separators/headers:** replace plain rules with CP437 double-line + dither
  composites, e.g. `╔══[ ✧ STARFIELD ]══════▓▒░` — label boxed in the rule,
  trailing dither fade (`▓▒░`). One shared helper `bbs_rule(label, width)` in
  `animation/common.rs`, used by Arcade separators + the `[i]` dashboard
  region headers + castle board separators.
- **Half-block gradients:** where vertical resolution matters (castle tower
  fills, defrag columns, timeline band), use `▀▄` half-blocks to double
  effective vertical resolution — the classic ANSI trick.
- **Title bars:** Arcade + `[i]` title rows get a subtle BBS-style frame
  (`▄▄▄` underline / `░▒▓█ tt-toplike █▓▒░` bookends), still char-safe and
  width-clamped.
- All through `colors::rgb`/`hsv_to_rgb`, so grayskull themes it automatically
  (dither fades in grey, hot accents in pink).
- No right-side border characters (project TUI rule) — left/bottom only.

## Error handling / degradation

- Duel: no serving snapshot / non-Linux / nothing Ready → strip absent, header
  identical to today. Zero-width guards; balance clamped; NaN-safe.
- Dedup: standalone modes unchanged (chrome defaults on); Arcade strip
  truncates char-safe per width; many devices → strip truncates with `…`.
- Castle compact tier: boundary widths (7/8/14/15 per-device cols) tested; no
  panic; particle budgets never zero.
- `bbs_rule`: label longer than width → char-safe truncation; width 0 → empty.

## Testing (pure / host-runnable)

- `duel_balance` range/clamp/NaN; lunge triggers (power spike → hero,
  completed_delta → snake); idle → resting states.
- Dedup: arcade render contains exactly ONE occurrence of a device's `W`
  readout (regression: previously 3); standalone castle/starfield renders
  unchanged (snapshot of header line).
- Castle tier dispatch table; compact column width budget; boundary no-panic.
- `bbs_rule` width/truncation/dither-tail; render smokes via TestBackend.

## Out of scope

- A playable/interactive duel (it's an instrument, not a game).
- Persisted duel score; multi-server duels (first Ready service only).
- Reworking per-view content art beyond the shared chrome/separators (the
  block-ramp shading from the earlier pass stays as-is).
- The buttoning-up waves (dead text-view path, vendor/, docs, CI) — queued
  separately right after this ships.
