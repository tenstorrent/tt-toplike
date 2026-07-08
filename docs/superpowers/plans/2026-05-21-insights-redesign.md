# Insights Screen Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Redesign the Insights screen with auto-column chip layout, temperature heatmap portrait colors, and a modal kill-process dialog.

**Architecture:** Two files only — `chip_portrait.rs` (char set + color helpers) and `mod.rs` (layout + dialog + key handlers). Each task is independently testable; Tasks 1-3 are pure portrait changes; Task 4 is pure layout; Task 5 is pure UX.

**Tech Stack:** Rust, Ratatui 0.30, `linux-procfs` feature gate, `libc` for SIGTERM/SIGKILL, `unicode-width` crate.

---

## Files Changed

| File | Change |
|------|--------|
| `src/ui/tui/chip_portrait.rs` | New char set (`▪` DRAM, `╋` PCIe), `░` Tensix floor, `lerp_rgb`/`tensix_temp_rgb`/`dram_temp_rgb` helpers, per-cell heatmap colors in `build_portrait_lines` |
| `src/ui/tui/mod.rs` | Auto-column layout helper + `render_device_panels` 2-D pass; `render_kill_dialog`; updated `KillConfirmState`; updated key handlers; simplified footer |

---

## Task 1: Update DRAM and PCIe characters

**Files:**
- Modify: `src/ui/tui/chip_portrait.rs` (lines 103-176 `build_portrait_rows`, lines 452-475 tests)

### Context

`build_portrait_rows` currently emits `'D'` for DRAM cells and `'P'` for PCIe cells. Two tests verify these characters and must be updated together with the implementation.

- [ ] **Step 1: Update the failing tests first**

In `chip_portrait.rs`, update `test_bh_dram_rows_show_d` (line 452) and `test_bh_pcie_shows_p` (line 464):

```rust
#[test]
fn test_bh_dram_rows_show_d() {
    let device = make_device(Architecture::Blackhole);
    let smbus  = make_smbus(Architecture::Blackhole);
    let telem  = make_telemetry();
    let rows = build_portrait_rows(&device, &telem, Some(&smbus), 0);
    // row 0 col 1 (non-ETH, non-PCIe) should be '▪'
    let row0_chars: Vec<char> = rows[0].chars().collect();
    assert_eq!(row0_chars[1], '▪', "BH row=0 col=1 should be DRAM '▪'");
    let row11_chars: Vec<char> = rows[11].chars().collect();
    assert_eq!(row11_chars[1], '▪', "BH row=11 col=1 should be DRAM '▪'");
}

#[test]
fn test_bh_pcie_shows_p() {
    let device = make_device(Architecture::Blackhole);
    let smbus  = make_smbus(Architecture::Blackhole);
    let telem  = make_telemetry();
    let rows = build_portrait_rows(&device, &telem, Some(&smbus), 0);
    // col 8, non-DRAM rows (1-10) should be '╋'
    for row_idx in 1..11 {
        let chars: Vec<char> = rows[row_idx].chars().collect();
        assert_eq!(chars[8], '╋', "BH PCIe col=8 row={} should be '╋'", row_idx);
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```
cargo test --lib -p tt-toplike-rs -- chip_portrait 2>&1 | grep -E "FAILED|PASSED|test_bh"
```

Expected: `test_bh_dram_rows_show_d` and `test_bh_pcie_shows_p` FAILED.

- [ ] **Step 3: Change chars in `build_portrait_rows`**

In `build_portrait_rows` (around line 129), replace the two character assignments:

```rust
// OLD:
CoreType::Pcie => 'P',
// ...
CoreType::Dram => 'D',

// NEW:
CoreType::Pcie => '╋',
// ...
CoreType::Dram => '▪',
```

Also update the doc-comment on `build_portrait_rows` (line ~105) to replace `D` with `▪` and `P` with `╋`:

```rust
/// Only single-column chars are used: ▓ ▒ ░ · ▪ ● ╋ (all width-1 per unicode-width crate).
```

- [ ] **Step 4: Run tests to confirm they pass**

```
cargo test --lib -p tt-toplike-rs -- chip_portrait 2>&1 | grep -E "FAILED|PASSED|test_bh"
```

Expected: all `chip_portrait` tests PASSED.

- [ ] **Step 5: Commit**

```bash
git add src/ui/tui/chip_portrait.rs
git commit -m "feat(ui): change DRAM char to ▪ and PCIe char to ╋ in chip portrait"
```

---

## Task 2: Add color helpers and Tensix minimum floor

**Files:**
- Modify: `src/ui/tui/chip_portrait.rs` (add functions after line 76 `gddr_color`, change `tensix_char`)

### Context

`tensix_char` currently returns `'·'` at low activity, making idle non-harvested cells indistinguishable from harvested cells and preventing the heatmap color from showing. Three new helper functions provide the interpolated RGB colors used in Task 3.

- [ ] **Step 1: Write tests for the new helpers**

Add to the `#[cfg(test)]` block in `chip_portrait.rs`:

```rust
#[test]
fn test_lerp_rgb_midpoint() {
    let result = lerp_rgb([0, 0, 0], [100, 200, 100], 0.5);
    assert_eq!(result, [50, 100, 50]);
}

#[test]
fn test_lerp_rgb_clamp() {
    // t outside [0,1] must clamp
    let a = lerp_rgb([10, 20, 30], [90, 80, 70], -1.0);
    assert_eq!(a, [10, 20, 30]);
    let b = lerp_rgb([10, 20, 30], [90, 80, 70], 2.0);
    assert_eq!(b, [90, 80, 70]);
}

#[test]
fn test_tensix_temp_rgb_cold() {
    // ≤40°C → teal #4FD1C5
    let [r, g, b] = tensix_temp_rgb(20.0);
    assert_eq!([r, g, b], [79, 209, 197]);
}

#[test]
fn test_tensix_temp_rgb_hot() {
    // >80°C → red #FF6B6B
    let [r, g, b] = tensix_temp_rgb(90.0);
    assert_eq!([r, g, b], [255, 107, 107]);
}

#[test]
fn test_tensix_temp_rgb_midrange() {
    // exactly 65°C → gold #F4C471
    let [r, g, b] = tensix_temp_rgb(65.0);
    assert_eq!([r, g, b], [244, 196, 113]);
}

#[test]
fn test_dram_temp_rgb_cold() {
    // ≤40°C → blue #3B82F6
    let [r, g, b] = dram_temp_rgb(30.0);
    assert_eq!([r, g, b], [59, 130, 246]);
}

#[test]
fn test_dram_temp_rgb_hot() {
    // >60°C → approaching red
    let [r, g, b] = dram_temp_rgb(80.0);
    // should be warmer than amber
    assert!(r > 200, "hot DRAM should have high red channel");
}

#[test]
fn test_tensix_char_idle_non_harvested_is_floor() {
    // activity=0.0 → minimum '░' (not '·')
    assert_eq!(tensix_char(0.0), '░');
    assert_eq!(tensix_char(0.04), '░');
}

#[test]
fn test_tensix_char_active() {
    assert_eq!(tensix_char(0.40), '▒');
    assert_eq!(tensix_char(0.75), '▓');
}
```

- [ ] **Step 2: Run to confirm they fail**

```
cargo test --lib -p tt-toplike-rs -- chip_portrait::tests::test_lerp 2>&1
cargo test --lib -p tt-toplike-rs -- chip_portrait::tests::test_tensix_temp 2>&1
cargo test --lib -p tt-toplike-rs -- chip_portrait::tests::test_dram_temp 2>&1
cargo test --lib -p tt-toplike-rs -- chip_portrait::tests::test_tensix_char_idle 2>&1
```

Expected: all FAILED (functions don't exist yet; `test_tensix_char_idle_non_harvested_is_floor` fails because current function returns `'·'`).

- [ ] **Step 3: Add the three color helpers and fix `tensix_char`**

After the existing `gddr_color` function (around line 76), add:

```rust
/// Linear interpolation between two RGB triples. `t` is clamped to [0, 1].
pub(crate) fn lerp_rgb(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t) as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t) as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t) as u8,
    ]
}

/// Temperature-to-RGB for Tensix cells (ASIC temperature heatmap).
///
/// ≤40°C → teal #4FD1C5, 40–65°C → lerp to gold #F4C471,
/// 65–80°C → lerp to red #FF6B6B, >80°C → red.
pub(crate) fn tensix_temp_rgb(temp_c: f32) -> [u8; 3] {
    const TEAL: [u8; 3] = [79, 209, 197];
    const GOLD: [u8; 3] = [244, 196, 113];
    const RED:  [u8; 3] = [255, 107, 107];
    if temp_c <= 40.0 {
        TEAL
    } else if temp_c <= 65.0 {
        lerp_rgb(TEAL, GOLD, (temp_c - 40.0) / 25.0)
    } else if temp_c <= 80.0 {
        lerp_rgb(GOLD, RED, (temp_c - 65.0) / 15.0)
    } else {
        RED
    }
}

/// Temperature-to-RGB for DRAM cells (GDDR temperature heatmap).
///
/// ≤40°C → blue #3B82F6, 40–60°C → lerp to amber #F4C471, >60°C → lerp to red #FF6B6B.
pub(crate) fn dram_temp_rgb(temp_c: f32) -> [u8; 3] {
    const BLUE:  [u8; 3] = [59, 130, 246];
    const AMBER: [u8; 3] = [244, 196, 113];
    const RED:   [u8; 3] = [255, 107, 107];
    if temp_c <= 40.0 {
        BLUE
    } else if temp_c <= 60.0 {
        lerp_rgb(BLUE, AMBER, (temp_c - 40.0) / 20.0)
    } else {
        lerp_rgb(AMBER, RED, (temp_c - 60.0) / 20.0)
    }
}
```

Change `tensix_char` (line 63) so idle non-harvested cells show `░` instead of `·`:

```rust
fn tensix_char(activity: f32) -> char {
    if      activity >= 0.75 { '▓' }
    else if activity >= 0.40 { '▒' }
    else                     { '░' }  // minimum floor — harvested cells use '·' directly
}
```

- [ ] **Step 4: Run tests to confirm they pass**

```
cargo test --lib -p tt-toplike-rs -- chip_portrait 2>&1 | grep -E "FAILED|PASSED"
```

Expected: all PASSED (including `test_harvested_col_shows_dot` — harvested path uses `'·'` directly in `build_portrait_rows`, bypassing `tensix_char`).

- [ ] **Step 5: Commit**

```bash
git add src/ui/tui/chip_portrait.rs
git commit -m "feat(ui): add lerp_rgb/tensix_temp_rgb/dram_temp_rgb helpers, set ░ as Tensix floor"
```

---

## Task 3: Apply heatmap colors in `build_portrait_lines`

**Files:**
- Modify: `src/ui/tui/chip_portrait.rs` (lines 178-238 `build_portrait_lines`)

### Context

`build_portrait_lines` currently assigns static colors (Cyan for active Tensix, Yellow for `▒`, DarkGray for `░`, Blue for PCIe). Replace with RGB interpolation using the helpers from Task 2. Harvested `'·'` cells keep DarkGray+DIM.

- [ ] **Step 1: Write a compile test to verify the new colors are used**

Add to the test block (these compile-test the functions introduced in Task 2 are accessible and that the feature gate works):

```rust
#[cfg(feature = "tui")]
#[test]
fn test_build_portrait_lines_uses_rgb() {
    // Non-harvested idle Tensix cell must use a non-DarkGray color at any temp.
    // At 20°C the cell should be teal RGB(79,209,197) — not Color::Cyan.
    // We can only test build_portrait_rows here (build_portrait_lines is tui-only),
    // but we verify the floor char is '░' so the heatmap can show through.
    let device = make_device(Architecture::Blackhole);
    let smbus  = make_smbus(Architecture::Blackhole);
    let telem  = Telemetry { power: Some(1.0), asic_temperature: Some(20.0), ..Telemetry::new() };
    let rows   = build_portrait_rows(&device, &telem, Some(&smbus), 0);
    // A non-ETH, non-DRAM, non-PCIe, non-harvested Tensix cell: col=1, row=1
    let ch = rows[1].chars().nth(1).unwrap();
    assert_eq!(ch, '░', "idle non-harvested Tensix should show '░' floor");
}
```

- [ ] **Step 2: Run to confirm it passes** (it should already pass after Task 2)

```
cargo test --lib -p tt-toplike-rs -- chip_portrait::tests::test_build_portrait_lines_uses_rgb 2>&1
```

Expected: PASSED.

- [ ] **Step 3: Rewrite `build_portrait_lines`**

Replace the entire function body (keeping the function signature unchanged):

```rust
#[cfg(feature = "tui")]
pub fn build_portrait_lines<'a>(
    device: &Device,
    telemetry: &Telemetry,
    smbus: Option<&SmbusTelemetry>,
    tick: u64,
) -> Vec<Line<'a>> {
    let rows_strs = build_portrait_rows(device, telemetry, smbus, tick);
    let (cols, _) = portrait_dims(device.architecture);

    // Pre-compute ASIC temp → Tensix heatmap color (same for all Tensix cells on this chip)
    let asic_temp = telemetry.temp_c();
    let [tr, tg, tb] = tensix_temp_rgb(asic_temp);
    let tensix_color = Color::Rgb(tr, tg, tb);

    // Per-column DRAM temperature color via GDDR temp interpolation
    let dram_color_for_col = |col: usize| -> Color {
        let pair_idx = (col / 4).min(3);
        let temp = smbus
            .and_then(|s| s.gddr_temps.get(pair_idx).and_then(|p| p.as_ref()))
            .map(|p| p.0.iter().copied().fold(f32::NEG_INFINITY, f32::max))
            .unwrap_or(0.0);
        let [r, g, b] = dram_temp_rgb(temp);
        Color::Rgb(r, g, b)
    };

    rows_strs.into_iter().enumerate().map(|(row, row_str)| {
        let mut spans: Vec<Span<'a>> = Vec::with_capacity(cols);

        for (col, ch) in row_str.chars().enumerate() {
            let core_type = match device.architecture {
                Architecture::Blackhole => core_type_bh(col, row),
                _                       => core_type_wh(col, row),
            };

            let style = match (core_type, ch) {
                // PCIe: amber (same gold as hot-Tensix anchor)
                (CoreType::Pcie, _) =>
                    Style::default().fg(Color::Rgb(244, 196, 113)),
                // ETH: green when live, dim gray when down
                (CoreType::Eth, '●') =>
                    Style::default().fg(Color::Green),
                (CoreType::Eth, _) =>
                    Style::default().fg(Color::DarkGray),
                // DRAM: blue→amber→red by per-column GDDR temperature
                (CoreType::Dram, _) =>
                    Style::default().fg(dram_color_for_col(col)),
                // Tensix harvested ('·'): dim gray, no heatmap
                (CoreType::Tensix, '·') =>
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                // Tensix non-harvested (░▒▓): ASIC temperature heatmap color
                (CoreType::Tensix, _) =>
                    Style::default().fg(tensix_color),
            };

            spans.push(Span::styled(ch.to_string(), style));
        }

        Line::from(spans)
    }).collect()
}
```

Also update the doc-comment above `build_portrait_lines` to reflect the new color scheme:

```rust
/// Color scheme:
/// - Tensix ░▒▓: ASIC temperature heatmap (teal→gold→red)
/// - Tensix ·(harvested): DarkGray+DIM
/// - DRAM ▪: GDDR temperature heatmap (blue→amber→red)
/// - ETH ●(live): Green  ·(down): DarkGray
/// - PCIe ╋: Amber RGB(244,196,113)
```

- [ ] **Step 4: Build with TUI feature to catch compile errors**

```
cargo build --features tui --bin tt-toplike-tui 2>&1 | tail -20
```

Expected: compiles cleanly (0 errors).

- [ ] **Step 5: Run all chip portrait tests**

```
cargo test --lib -p tt-toplike-rs -- chip_portrait 2>&1 | grep -E "FAILED|PASSED|ok"
```

Expected: all PASSED.

- [ ] **Step 6: Commit**

```bash
git add src/ui/tui/chip_portrait.rs
git commit -m "feat(ui): apply ASIC/GDDR temperature heatmap colors in chip portrait"
```

---

## Task 4: Auto-column layout for device panels

**Files:**
- Modify: `src/ui/tui/mod.rs` (add `device_panels_height` helper; update `render_insights` constraint; rewrite `render_device_panels` inner loop)

### Context

`render_device_panels` currently iterates devices top-to-bottom, producing `15 × N` rows regardless of terminal width. The new version tiles panels left-to-right, wrapping when the row is full.

Panel width formula (matching existing code):
- BH: `(17+1) + 1 + 31 = 50` cols
- WH: `(10+1) + 1 + 31 = 43` cols

`cols_per_row = max(1, area.width / panel_w)`  
`row_count = ceil(n / cols_per_row)`  
Height allocated = `row_count × (panel_h + 1)` = `row_count × 15`

- [ ] **Step 1: Add `device_panels_height` helper function**

Add this function anywhere before `render_insights` in `mod.rs`:

```rust
/// Compute the vertical space required for the device panel grid at the given terminal width.
///
/// Panel width is derived from the first device's architecture. If no devices, returns 15.
#[cfg(feature = "linux-procfs")]
fn device_panels_height(
    devices: &[crate::models::Device],
    area_width: u16,
) -> u16 {
    use crate::ui::tui::chip_portrait::portrait_dims;
    let n = devices.len().max(1);
    let arch = devices.first().map(|d| d.architecture).unwrap_or(crate::models::Architecture::Blackhole);
    let (portrait_cols, _) = portrait_dims(arch);
    let panel_w = portrait_cols as u16 + 33; // left-border(1) + gap(1) + stats(31)
    let cols_per_row = (area_width / panel_w).max(1) as usize;
    let row_count = (n + cols_per_row - 1) / cols_per_row;
    (row_count as u16) * 15 // panel_h(14) + inter-row gap(1)
}
```

- [ ] **Step 2: Update the height constraint in `render_insights`**

In `render_insights` (around line 1812), replace the fixed constraint:

```rust
// OLD:
Constraint::Length((15 * n) as u16),

// NEW:
Constraint::Length(device_panels_height(devices, area.width)),
```

- [ ] **Step 3: Rewrite `render_device_panels` to use 2-D grid**

Replace the entire `render_device_panels` function body (keep the function signature unchanged). The key change is computing `col_idx`/`row_idx` from `panel_idx` and computing both `x_offset` and `y_offset` from them:

```rust
#[cfg(feature = "linux-procfs")]
fn render_device_panels(
    f: &mut Frame,
    area: Rect,
    backend: &Box<dyn TelemetryBackend>,
    _engine: &crate::workload::InferenceEngine,
    tick: u64,
) {
    use crate::ui::tui::chip_portrait::{portrait_dims, render_chip_portrait};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;

    let devices = backend.devices();
    if devices.is_empty() { return; }

    let panel_h: u16 = 14; // 12 portrait rows + 1 title row + 1 bottom border
    let inter_col_gap: u16 = 1;
    let inter_row_gap: u16 = 1;

    // Panel width from first device's architecture (uniform grid)
    let (portrait_cols, _) = portrait_dims(devices[0].architecture);
    let portrait_w = portrait_cols as u16 + 1; // left border ║
    let stats_w    = 31_u16;                    // border ║ + 30 content
    let gap_col    = 1_u16;                     // gap between portrait and stats
    let panel_w    = portrait_w + gap_col + stats_w;

    let cols_per_row = (area.width / panel_w).max(1) as usize;

    for (panel_idx, device) in devices.iter().enumerate() {
        let col_idx = (panel_idx % cols_per_row) as u16;
        let row_idx = (panel_idx / cols_per_row) as u16;

        let x_offset = area.x + col_idx * (panel_w + inter_col_gap);
        let y_offset = area.y + row_idx * (panel_h + inter_row_gap);

        // Skip panels that overflow the allocated area
        if y_offset + panel_h > area.y + area.height { break; }
        if x_offset + panel_w > area.x + area.width  { continue; }

        let idx      = device.index;
        let telemetry = backend.telemetry(idx);
        let smbus     = backend.smbus_telemetry(idx);

        // Per-device portrait dims (may differ in mixed-arch systems)
        let (p_cols, _) = portrait_dims(device.architecture);
        let p_portrait_w = p_cols as u16 + 1;
        let panel_rect = Rect { x: x_offset, y: y_offset, width: panel_w, height: panel_h };

        // ── Left: portrait with left border ──────────────────────────────────
        let left_border_rect = Rect { x: panel_rect.x, y: panel_rect.y, width: 1, height: panel_h };
        let border_lines: Vec<Line> = (0..panel_h as usize).map(|r| {
            let ch = if r == 0 { "╔" } else if r == panel_h as usize - 1 { "╚" } else { "║" };
            Line::from(Span::styled(ch, Style::default().fg(Color::DarkGray)))
        }).collect();
        f.render_widget(Paragraph::new(border_lines), left_border_rect);

        let portrait_rect = Rect {
            x: panel_rect.x + 1,
            y: panel_rect.y + 1,
            width:  p_cols as u16,
            height: panel_h.saturating_sub(2),
        };
        if let Some(telem) = telemetry {
            render_chip_portrait(f, portrait_rect, device, telem, smbus, tick);
        }

        // ── Right: stats sidebar ──────────────────────────────────────────────
        let stats_x = panel_rect.x + p_portrait_w + gap_col;
        let stats_border_rect = Rect { x: stats_x, y: panel_rect.y, width: 1, height: panel_h };
        let stats_border: Vec<Line> = (0..panel_h as usize).map(|r| {
            let ch = if r == 0 { "╔" } else if r == panel_h as usize - 1 { "╚" } else { "║" };
            Line::from(Span::styled(ch, Style::default().fg(Color::DarkGray)))
        }).collect();
        f.render_widget(Paragraph::new(stats_border), stats_border_rect);

        let content_x = stats_x + 1;
        let content_w = stats_w.saturating_sub(1);
        let content_rect = Rect { x: content_x, y: panel_rect.y, width: content_w, height: panel_h };

        let mut stat_lines: Vec<Line> = Vec::new();

        // Title row
        let arch_short = device.architecture.abbrev();
        let title = format!("Dev {:2}  {} {}", idx, arch_short,
            &device.board_type[..device.board_type.len().min(8)]);
        stat_lines.push(Line::from(Span::styled(
            format!("{:<28}", &title[..title.len().min(28)]),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
        )));

        // Power row
        let power_w_val = telemetry.map(|t| t.power_w()).unwrap_or(0.0);
        let tdp = device.limits.as_ref().and_then(|l| l.tdp_limit).unwrap_or(300.0);
        let power_frac = (power_w_val / tdp).min(1.0);
        let bar_filled = (power_frac * 10.0) as usize;
        let power_bar: String = (0..10).map(|i| if i < bar_filled { '█' } else { '░' }).collect();
        let power_color = if power_frac > 0.85 { Color::Red }
                         else if power_frac > 0.6 { Color::Yellow }
                         else { Color::Green };
        stat_lines.push(Line::from(vec![
            Span::styled(format!("{:<8}", "Power"), Style::default().fg(Color::DarkGray)),
            Span::styled(power_bar, Style::default().fg(power_color)),
            Span::styled(format!(" {:5.0}W", power_w_val), Style::default().fg(Color::White)),
        ]));

        // ETH row
        let eth_live_mask = smbus.and_then(|s| s.eth_live_status).unwrap_or(0);
        let eth_total: u32 = match device.architecture {
            crate::models::Architecture::Blackhole => 24,
            crate::models::Architecture::Wormhole  => 20,
            _                                       => 4,
        };
        let eth_live_count = eth_live_mask.count_ones();
        let eth_dots: String = (0..eth_total.min(8)).map(|i| {
            if (eth_live_mask >> i) & 1 == 1 { '●' } else { '·' }
        }).collect();
        let eth_color = if eth_live_count == eth_total { Color::Green } else { Color::Yellow };
        stat_lines.push(Line::from(vec![
            Span::styled(format!("{:<8}", "ETH"), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{:<8}", eth_dots), Style::default().fg(eth_color)),
            Span::styled(format!(" {}/{} live", eth_live_count, eth_total), Style::default().fg(Color::White)),
        ]));

        // GDDR temp row
        if let Some(s) = smbus {
            let temps: Vec<f32> = s.gddr_temps.iter()
                .filter_map(|p| p.as_ref())
                .flat_map(|p| p.0.iter().copied())
                .filter(|&t| t > 0.0)
                .collect();
            if !temps.is_empty() {
                let t_min = temps.iter().copied().fold(f32::INFINITY, f32::min);
                let t_max = temps.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let max_color = if t_max > 70.0 { Color::Red }
                               else if t_max > 50.0 { Color::Yellow }
                               else { Color::Cyan };
                stat_lines.push(Line::from(vec![
                    Span::styled(format!("{:<8}", "GDDR T"), Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{:.0}°→{:.0}°C", t_min, t_max), Style::default().fg(max_color)),
                ]));
            }
        }

        // ASIC temp row
        let asic_temp = telemetry.map(|t| t.temp_c()).unwrap_or(0.0);
        let thm_limit = device.limits.as_ref().and_then(|l| l.thm_limit).unwrap_or(105.0);
        let temp_color = crate::ui::colors::temp_color(asic_temp);
        stat_lines.push(Line::from(vec![
            Span::styled(format!("{:<8}", "Temp"), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{:.1}°C", asic_temp), Style::default().fg(temp_color)),
            Span::styled(format!(" (lim {:.0}°C)", thm_limit), Style::default().fg(Color::DarkGray)),
        ]));

        // Fan RPM row
        if let Some(rpm_str) = smbus.and_then(|s| s.fan_speed.as_deref()) {
            if let Ok(rpm) = rpm_str.trim().parse::<u32>() {
                if rpm > 0 {
                    stat_lines.push(Line::from(vec![
                        Span::styled(format!("{:<8}", "Fan"), Style::default().fg(Color::DarkGray)),
                        Span::styled(format!("{} RPM", rpm), Style::default().fg(Color::White)),
                    ]));
                }
            }
        }

        // Firmware row
        let fw_ver = device.firmwares.as_ref()
            .and_then(|fw| fw.fw_bundle_version.as_deref())
            .unwrap_or("—");
        let fw_short = fw_ver.trim_start_matches("fw_pack-");
        let fw_display = &fw_short[..fw_short.len().min(22)];
        stat_lines.push(Line::from(vec![
            Span::styled(format!("{:<8}", "FW"), Style::default().fg(Color::DarkGray)),
            Span::styled(fw_display.to_string(), Style::default().fg(Color::White)),
        ]));

        // Pad to panel_h-1 rows then bottom separator
        while stat_lines.len() < panel_h as usize - 1 {
            stat_lines.push(Line::raw(""));
        }
        stat_lines.push(Line::from(Span::styled(
            "═".repeat(content_w as usize),
            Style::default().fg(Color::DarkGray),
        )));

        f.render_widget(Paragraph::new(stat_lines), content_rect);
    }
}
```

- [ ] **Step 4: Build and confirm it compiles**

```
cargo build --features tui,linux-procfs --bin tt-toplike-tui 2>&1 | tail -10
```

Expected: 0 errors.

- [ ] **Step 5: Run all existing tests**

```
cargo test --lib -p tt-toplike-rs 2>&1 | grep -E "FAILED|test result"
```

Expected: 0 FAILED.

- [ ] **Step 6: Commit**

```bash
git add src/ui/tui/mod.rs
git commit -m "feat(ui): auto-column layout for Insights device panels, all devices visible at wide terminals"
```

---

## Task 5: Kill process modal dialog

**Files:**
- Modify: `src/ui/tui/mod.rs` (update `KillConfirmState`; add `render_kill_dialog`; update key handlers; update `render_insights` to overlay dialog; simplify `render_insights_footer`)

### Context

The current kill flow changes the footer text (hard to notice). Replace with a centered modal. `k` opens the dialog; `enter` inside sends SIGTERM; `K` anywhere sends SIGKILL immediately; `n`/`Esc` cancel.

The dialog follows CLAUDE.md: left-side and bottom borders only (`╔`/`║`/`╚`), no right-side border characters.

- [ ] **Step 1: Add `device_idx` to `KillConfirmState`, remove `signal`**

In `mod.rs` replace lines 44-48:

```rust
// OLD:
#[cfg(feature = "linux-procfs")]
#[derive(Debug, Clone)]
struct KillConfirmState {
    pid:    i32,
    name:   String,
    signal: i32,   // libc::SIGTERM (15) or libc::SIGKILL (9)
}

// NEW:
#[cfg(feature = "linux-procfs")]
#[derive(Debug, Clone)]
struct KillConfirmState {
    pid:        i32,
    name:       String,
    device_idx: usize,
}
```

- [ ] **Step 2: Add `render_kill_dialog` function**

Add this function after `render_insights_footer` (around line 2070 in the original file):

```rust
/// Render a centered kill-confirmation modal dialog over the current frame.
///
/// Uses `Clear` to erase the background, then draws a left-border-only dialog
/// (no right-side border characters per CLAUDE.md).
#[cfg(feature = "linux-procfs")]
fn render_kill_dialog(f: &mut Frame, area: Rect, kc: &KillConfirmState) {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Clear, Paragraph};

    const DIALOG_W: u16 = 42;
    const DIALOG_H: u16 = 8;

    let x = area.x.saturating_add((area.width.saturating_sub(DIALOG_W)) / 2);
    let y = area.y.saturating_add((area.height.saturating_sub(DIALOG_H)) / 2);
    let dialog_rect = Rect {
        x,
        y,
        width:  DIALOG_W.min(area.width),
        height: DIALOG_H.min(area.height),
    };

    // Erase the background behind the dialog
    f.render_widget(Clear, dialog_rect);

    let content_w = (DIALOG_W.saturating_sub(1)) as usize; // ║ takes 1 col
    let name_short = truncate(&kc.name, 14);
    let dev_label  = format!("Dev {}", kc.device_idx);

    let title_pad = content_w.saturating_sub("╔ Kill process? ".len());
    let bottom_pad = content_w;

    let lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled("╔ Kill process? ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("═".repeat(title_pad), Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("║  ", Style::default().fg(Color::DarkGray)),
            Span::styled(name_short.to_string(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" · PID {} · {}", kc.pid, dev_label), Style::default().fg(Color::Gray)),
        ]),
        Line::from(Span::styled("║", Style::default().fg(Color::DarkGray))),
        Line::from(vec![
            Span::styled("║  ", Style::default().fg(Color::DarkGray)),
            Span::styled("enter", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::styled("    silence  (SIGTERM)", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("║  ", Style::default().fg(Color::DarkGray)),
            Span::styled("K", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::styled("        destroy  (SIGKILL)", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("║  ", Style::default().fg(Color::DarkGray)),
            Span::styled("n · esc", Style::default().fg(Color::Gray)),
            Span::styled("  cancel", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(Span::styled("║", Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled(
            format!("╚{}", "═".repeat(bottom_pad)),
            Style::default().fg(Color::DarkGray),
        )),
    ];

    f.render_widget(Paragraph::new(lines), dialog_rect);
}
```

Add `Clear` to the existing ratatui import in `mod.rs`. Find the line (around line 35):
```rust
widgets::{Block, BorderType, Borders, Paragraph, Row, Table},
```
And add `Clear`:
```rust
widgets::{Block, BorderType, Borders, Clear, Paragraph, Row, Table},
```

- [ ] **Step 3: Call `render_kill_dialog` from `render_insights`**

At the end of `render_insights`, after the `render_insights_footer` call, add:

```rust
render_insights_footer(f, chunks[3], kill_confirm);
// Kill modal overlays the full screen area
if let Some(ref kc) = kill_confirm {
    render_kill_dialog(f, area, kc);
}
```

- [ ] **Step 4: Simplify `render_insights_footer` (always show nav hints)**

Replace the entire body of `render_insights_footer` with only the nav hints (remove the `if let Some(kc) = kill_confirm` branch):

```rust
#[cfg(feature = "linux-procfs")]
fn render_insights_footer(
    f: &mut Frame,
    area: Rect,
    _kill_confirm: Option<&KillConfirmState>,
) {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    let line = Line::from(vec![
        Span::raw("  "),
        Span::styled("\u{2191}\u{2193}", Style::default().fg(Color::White)),
        Span::raw("  move"),
        Span::raw("  \u{00B7}  "),
        Span::styled("k", Style::default().fg(Color::Yellow)),
        Span::raw("  silence"),
        Span::raw("  \u{00B7}  "),
        Span::styled("K", Style::default().fg(Color::Red)),
        Span::raw("  destroy now"),
        Span::raw("  \u{00B7}  "),
        Span::styled("v", Style::default().fg(Color::Cyan)),
        Span::raw("  next view"),
        Span::raw("  \u{00B7}  "),
        Span::styled("g", Style::default().fg(Color::Cyan)),
        Span::raw("  grid"),
        Span::raw("  \u{00B7}  "),
        Span::styled("q", Style::default().fg(Color::DarkGray)),
        Span::raw("  leave"),
    ]);

    f.render_widget(Paragraph::new(line), area);
}
```

- [ ] **Step 5: Update key handlers in the event loop**

Replace the four Insights-mode handler arms (lines ~425-472) with:

```rust
// Arrow keys: navigate process list (blocked while dialog is open)
#[cfg(feature = "linux-procfs")]
KeyCode::Up if display_mode == DisplayMode::Insights => {
    if kill_confirm.is_none() {
        process_cursor = process_cursor.saturating_sub(1);
    }
}
#[cfg(feature = "linux-procfs")]
KeyCode::Down if display_mode == DisplayMode::Insights => {
    if kill_confirm.is_none() {
        let max = flat_process_list(&process_monitor).len().saturating_sub(1);
        process_cursor = (process_cursor + 1).min(max);
    }
}

// 'k' → open kill dialog (SIGTERM)
#[cfg(feature = "linux-procfs")]
KeyCode::Char('k') if display_mode == DisplayMode::Insights => {
    if kill_confirm.is_none() {
        if let Some(proc) = flat_process_list(&process_monitor).get(process_cursor) {
            kill_confirm = Some(KillConfirmState {
                pid:        proc.pid,
                name:       proc.name.clone(),
                device_idx: proc.device_indices.first().copied().unwrap_or(0),
            });
        }
    }
}

// 'K' → SIGKILL immediately (no dialog outside); SIGKILL inside dialog
#[cfg(feature = "linux-procfs")]
KeyCode::Char('K') if display_mode == DisplayMode::Insights => {
    if let Some(ref kc) = kill_confirm {
        let _ = crate::workload::process_monitor::kill_pid(kc.pid, libc::SIGKILL);
        kill_confirm = None;
    } else if let Some(proc) = flat_process_list(&process_monitor).get(process_cursor) {
        let _ = crate::workload::process_monitor::kill_pid(proc.pid, libc::SIGKILL);
    }
}

// Enter → SIGTERM (confirm dialog)
#[cfg(feature = "linux-procfs")]
KeyCode::Enter if display_mode == DisplayMode::Insights => {
    if let Some(ref kc) = kill_confirm {
        let _ = crate::workload::process_monitor::kill_pid(kc.pid, libc::SIGTERM);
        kill_confirm = None;
    }
}

// 'y' → alias for Enter (backward compat)
#[cfg(feature = "linux-procfs")]
KeyCode::Char('y') if display_mode == DisplayMode::Insights => {
    if let Some(ref kc) = kill_confirm {
        let _ = crate::workload::process_monitor::kill_pid(kc.pid, libc::SIGTERM);
        kill_confirm = None;
    }
}

// 'n' → cancel dialog
#[cfg(feature = "linux-procfs")]
KeyCode::Char('n') if display_mode == DisplayMode::Insights => {
    kill_confirm = None;
}
```

Note: `Esc` (line 348) already cancels `kill_confirm` or quits — no change needed there.

- [ ] **Step 6: Build and confirm it compiles**

```
cargo build --features tui,linux-procfs --bin tt-toplike-tui 2>&1 | tail -10
```

Expected: 0 errors.

- [ ] **Step 7: Run all tests**

```
cargo test --lib -p tt-toplike-rs 2>&1 | grep -E "FAILED|test result"
```

Expected: 0 FAILED.

- [ ] **Step 8: Commit**

```bash
git add src/ui/tui/mod.rs
git commit -m "feat(ui): kill process modal dialog — k opens dialog, enter=SIGTERM, K=SIGKILL, n/esc=cancel"
```

---

## Spec Coverage Self-Review

| Spec requirement | Task |
|-----------------|------|
| DRAM char `▪` | Task 1 |
| PCIe char `╋` | Task 1 |
| Tensix `░` minimum floor (non-harvested idle) | Task 2 |
| `lerp_rgb` helper | Task 2 |
| `tensix_temp_rgb` (teal→gold→red) | Task 2 |
| `dram_temp_rgb` (blue→amber→red) | Task 2 |
| Tensix heatmap color from ASIC temp | Task 3 |
| DRAM heatmap color from GDDR temp per column | Task 3 |
| PCIe amber color | Task 3 |
| ETH green/gray unchanged | Task 3 |
| Harvested cells keep DarkGray+DIM | Task 3 |
| Auto-column layout (`cols_per_row = terminal_width / panel_w`) | Task 4 |
| 2-D panel grid (outer loop rows, inner loop cols) | Task 4 |
| `render_insights` height constraint from `device_panels_height` | Task 4 |
| `k` → open kill dialog | Task 5 |
| `K` outside dialog → SIGKILL immediately | Task 5 |
| `enter` in dialog → SIGTERM, close | Task 5 |
| `K` in dialog → SIGKILL, close | Task 5 |
| `n`/`Esc` in dialog → cancel | Task 5 |
| Dialog uses `ratatui::widgets::Clear` | Task 5 |
| Footer reverts to nav hints only | Task 5 |
| No right-side border chars (CLAUDE.md) | Task 5 |
