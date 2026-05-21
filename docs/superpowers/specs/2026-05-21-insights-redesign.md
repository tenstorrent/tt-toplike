# Insights Screen Redesign

**Date:** 2026-05-21
**Status:** Approved

## Problem

The current Insights screen stacks device panels vertically. Four BH devices × 14 rows each = 56 rows of device content before the process panel appears, so all four chips are never simultaneously visible. Meanwhile the horizontal axis is nearly entirely wasted — each panel is only 50 of ~180+ available columns.

The chip portrait also undersells the hardware: DRAM rows show `D` (a letter), the PCIe cell shows `P`, and Tensix cells are uniformly gray at idle because the activity character threshold is never met at low power. Temperature information exists in the data but is not expressed visually.

## Goals

1. All devices visible simultaneously without scrolling.
2. Chip portrait that conveys temperature and core-type identity at a glance — inspired by tensix-viz's per-cell color language.
3. Process kill workflow that uses a modal dialog rather than a footer-line prompt.

## Non-Goals

- No per-Tensix-core temperature (tt-smi does not expose it; chip-level ASIC temp is used).
- No changes to the visualisation modes (Starfield, Castle, Flow, Arcade).
- No changes to the stats sidebar content (power bar, ETH, GDDR, temp, FW).

---

## Design

### 1. Auto-column Layout

`render_device_panels` computes `cols_per_row` from terminal width at render time:

```
panel_w  = portrait_cols + 1 (left border) + 1 (gap) + 31 (stats sidebar)
         = 18 + 1 + 31 = 50  for BH
         = 11 + 1 + 31 = 43  for WH

cols_per_row = max(1, terminal_width / panel_w)
```

Devices are arranged left-to-right, wrapping to a new row after `cols_per_row` panels. For four BH devices on a 200-col terminal: `200 / 50 = 4` — all four on one row in 14 rows of height, leaving ~35 rows for the process panel and footer.

The panel grid replaces the current vertical-only loop with a two-dimensional pass:
- outer loop: rows of panels
- inner loop: columns within each row
- `x` offset advances by `panel_w + 1` (1-col gap between panels)
- `y` offset advances by `panel_h + 1` after each panel row

No new display mode or keybinding is needed; the layout adapts automatically on every render.

### 2. Chip Portrait — Character Set

Changes confined to `src/ui/tui/chip_portrait.rs`.

| Core type | Char | Notes |
|-----------|------|-------|
| Tensix (harvested) | `·` | dim gray, unchanged |
| Tensix (active) | `·` / `░` / `▒` / `▓` | by activity; minimum `░` when not harvested so heatmap color is always visible |
| DRAM | `▪` | replaces `D` |
| ETH live | `●` | green, unchanged |
| ETH down | `·` | dim gray, unchanged |
| PCIe | `╋` | replaces `P` |

**Minimum Tensix floor:** replace `tensix_char(activity)` threshold so non-harvested cells use `░` when `activity < 0.05` instead of `·`. Harvested cells keep `·`. This ensures the temperature heatmap color is always visible even on idle chips.

### 3. Chip Portrait — Temperature Heatmap Colors

`build_portrait_lines` receives `telemetry` and `smbus`. Colors are computed per-cell, not per-chip-type globally.

**Tensix cells** — interpolate by ASIC temperature:

| Range | Color |
|-------|-------|
| ≤ 40 °C | `#4FD1C5` teal |
| 40–65 °C | lerp teal → `#F4C471` gold |
| 65–80 °C | lerp gold → `#FF6B6B` red |
| > 80 °C | `#FF6B6B` red |

Harvested cells remain `DarkGray + DIM` regardless of temperature.

**DRAM cells** — interpolate by per-column GDDR temperature (available as `smbus.gddr_temps`). Separate blue-anchored palette keeps DRAM visually distinct from Tensix:

| Range | Color |
|-------|-------|
| ≤ 40 °C | `#3B82F6` blue |
| 40–60 °C | lerp blue → `#F4C471` amber |
| > 60 °C | `#FF6B6B` red |

Column-to-GDDR-pair mapping for BH: `pair_idx = col / 4` (already used in the existing `dram_color_for_col` helper — extend it to use the new palette and interpolation).

**ETH cells** — unchanged: `Color::Green` (live) / `Color::DarkGray` (down).

**PCIe cell** — `Color::Rgb(244, 196, 113)` amber (same gold as the hot-Tensix anchor).

**Helper:** add `fn temp_to_rgb(t: f32, cold: [u8;3], hot: [u8;3], mid: Option<([u8;3], f32)>) -> Color` in `chip_portrait.rs` for lerp-based interpolation. Keeps color math testable and contained.

### 4. Kill Process Modal Dialog

**Trigger:** `k` on a focused process row opens the modal. `K` on a focused process row skips the modal and sends SIGKILL immediately (existing "destroy" shortcut, unchanged).

**Current behaviour being replaced:** kill confirmation changes the footer line. New behaviour: a centered `Clear`+`Block` overlay rendered on top of the current frame.

**Dialog layout** (left-border only, ~42 cols wide, 8 rows tall, centered):

```
╔ Kill process? ══════════════════════
║  python3 · PID 11204 · Dev 0
║
║  enter    silence  (SIGTERM)
║  K        destroy  (SIGKILL)
║  n · esc  cancel
║
╚════════════════════════════════════
```

**Key bindings inside dialog:**

| Key | Action |
|-----|--------|
| `enter` | send SIGTERM, close dialog |
| `K` | send SIGKILL, close dialog |
| `n` / `Esc` | cancel, close dialog |

**Implementation:** add a `KillDialogState { pid, name, device_idx }` variant to the existing `KillConfirmState` (or replace it). `render_insights` checks for this state and, when present, calls `render_kill_dialog(f, area, state)` after the main layout. The dialog uses `ratatui::widgets::Clear` to erase the background before rendering the block.

The existing `render_insights_footer` reverts to showing only the navigation hints (no kill-confirmation text) since the dialog handles confirmation.

---

## Files Changed

| File | Change |
|------|--------|
| `src/ui/tui/chip_portrait.rs` | New char set, `temp_to_rgb` helper, minimum-floor `░`, per-cell heatmap colors |
| `src/ui/tui/mod.rs` | Auto-column layout in `render_device_panels`; `render_kill_dialog`; updated `KillConfirmState`; updated footer |

No new files. No changes outside the TUI module.

---

## Open Questions

None — all design decisions resolved.
