// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The Die 3D board scene: what to draw (`ChipInputs`), how big (`fit`) and the
//! drawing itself (`draw_board_scene`).
//!
//! Ported from `Scene.draw` in
//! `docs/superpowers/specs/assets/2026-10-29-die-3d/die-3d-resolution.html`.
//!
//! This is the tier 0 scene: only passive telemetry is available, so the glow
//! on each Tensix tile follows the chip-wide load and is an estimate. The view
//! must show [`ESTIMATE_LABEL`] while that is the case. [`ChipInputs::tile_flow`]
//! is the slot where measured per-tile data goes; when it is `Some`, every tile
//! reads its heat from it (see [`tile_value`]).

use super::layout::{tensix_mask_to_die_cols, DieLayout, Tile, TileKind};
use super::raster::{draw_block, Iso, PixelBuf};
use super::super_scene::mean_heat;
use crate::backend::TelemetryBackend;
use crate::models::{Architecture, Device};
use std::collections::HashMap;

/// Shown next to the view whenever the tile glow is not measured per tile.
pub const ESTIMATE_LABEL: &str = "per-tile glow: estimate from chip load";

// Palette from the mockup.
const TILE: [u8; 3] = [30, 82, 78];
const WARM: [u8; 3] = [246, 188, 66];
const HOT: [u8; 3] = [250, 81, 46];
const DRAM: [u8; 3] = [111, 171, 160];
const ETH: [u8; 3] = [143, 149, 224];
const MGMT: [u8; 3] = [122, 127, 154];
const N1: [u8; 3] = [116, 197, 223];
pub(super) const BG: [u8; 3] = [7, 25, 26];
const SLAB: [u8; 3] = [13, 38, 36];
const WHITE: [u8; 3] = [255, 255, 255];
/// Packets leaving the chip: the warm accent, so they differ from the blue
/// packets arriving.
const PACKET_OUT: [u8; 3] = WARM;

// Board grid. Each die sits in a band of `BOARD_BAND` rows: one row of Ethernet
// cages (die row -1), the 12 die rows, then the PCIe connector row (die row
// 12). The cages of one die and the connector of the next therefore sit side
// by side in the 2-row gap between the two dies' tiles. Columns carry two
// extra tile columns each side for the GDDR blocks.
pub(super) const MARGIN_X: usize = 2;
pub(super) const MARGIN_Y: usize = 1;
pub(super) const BOARD_COLS: usize = 17 + 2 * MARGIN_X;
pub(super) const BOARD_BAND: usize = 12 + 2;
/// Ethernet cages along the back edge, one per `eth_live` bit.
const ETH_CAGES: usize = 12;
/// Rows of GDDR blocks (die rows) on each side, two rows per memory chip.
const GDDR_ROWS: [i32; 4] = [0, 3, 6, 9];
/// Smallest terminal the scene is drawn on, in cells.
const MIN_CELLS: (usize, usize) = (100, 30);
/// Packets are drawn only above this rate (bytes per second); below it the
/// counter is idle noise.
const MIN_PACKET_BPS: f64 = 1000.0;
/// Rate at which the packet stream is densest (PCIe 5.0 x16, about 64 GB/s).
const FULL_PACKET_BPS: f64 = 64.0e9;
const MAX_PACKETS: usize = 6;

/// Everything the scene needs to know about one chip, already reduced to plain
/// numbers so drawing never touches a backend.
#[derive(Debug, Clone)]
pub struct ChipInputs {
    pub index: usize,
    pub arch: Architecture,
    /// Power over TDP, 0..1. `0.0` when either is unknown.
    pub load: f32,
    /// Bytes per second into the chip, `None` without PCIe counters.
    pub pcie_in_bps: Option<f64>,
    /// Bytes per second out of the chip.
    pub pcie_out_bps: Option<f64>,
    /// Hottest GDDR temperature mapped from 40..90 C onto 0..1.
    pub gddr_heat: Option<f32>,
    /// Bit `i` set when Ethernet port `i` has a live link.
    pub eth_live: u64,
    pub temp_c: Option<f32>,
    /// `enabled_tensix_col` mask: a clear bit is a harvested column.
    pub enabled_tensix_col: Option<u32>,
    /// Measured per-tile heat, keyed by noc0 coordinate, one value per NoC
    /// (0..1). Always `None` from [`chip_inputs`] in tier 0; tier 1 fills it.
    pub tile_flow: Option<HashMap<(u8, u8), [f32; 2]>>,
}

/// A chip's TDP in watts: SMBUS first, the tt-smi `limits` block second, `None`
/// when neither reports a positive value. Same rule as `TrainView::chip_tdp`,
/// which is private to that view.
fn chip_tdp(backend: &dyn TelemetryBackend, device: &Device) -> Option<f32> {
    backend
        .smbus_telemetry(device.index)
        .and_then(|s| s.tdp_watts())
        .or_else(|| device.limits.as_ref().and_then(|l| l.tdp_limit))
        .filter(|v| *v > 0.0)
}

/// Read one chip's scene inputs from a backend. Reads only data the backend
/// already holds, nothing new is requested from the device.
pub fn chip_inputs(backend: &dyn TelemetryBackend, device: &Device) -> ChipInputs {
    let idx = device.index;
    let telem = backend.telemetry(idx);
    let smbus = backend.smbus_telemetry(idx);
    let load = match (telem.map(|t| t.power_w()), chip_tdp(backend, device)) {
        (Some(p), Some(tdp)) if p.is_finite() => (p / tdp).clamp(0.0, 1.0),
        _ => 0.0,
    };
    let pcie = backend.pcie_bandwidth(idx);
    ChipInputs {
        index: idx,
        arch: device.architecture,
        load,
        pcie_in_bps: pcie.map(|p| p.rx_bytes_per_sec),
        pcie_out_bps: pcie.map(|p| p.tx_bytes_per_sec),
        gddr_heat: smbus
            .and_then(|s| s.max_gddr_temp_computed())
            .map(|c| ((c - 40.0) / 50.0).clamp(0.0, 1.0)),
        eth_live: smbus.and_then(|s| s.eth_live_status).unwrap_or(0),
        temp_c: telem.and_then(|t| t.asic_temperature),
        enabled_tensix_col: smbus.and_then(|s| s.enabled_tensix_col),
        tile_flow: None,
    }
}

/// Estimated heat of a tile at time `t`: a wave sweeping across the Tensix
/// columns, scaled by the chip load, over a small floor so an idle die is
/// still visible. This is invented motion shaped by real chip-wide load, not a
/// measurement of the tile. Other kinds of tile have no wave.
pub fn tile_heat(load: f32, tile: &Tile, t: f32) -> f32 {
    if tile.kind != TileKind::Tensix {
        return 0.0;
    }
    let cols = DieLayout::blackhole().cols as f32;
    let (x, y) = (tile.die.0 as f32, tile.die.1 as f32);
    let front = (t * 0.9).rem_euclid(cols + 6.0) - 3.0;
    let wave = (1.0 - (x - front).abs() / 4.0).max(0.0);
    let shimmer = 0.15 * (0.5 + 0.5 * (t * 5.0 + x * 1.7 + y * 2.3).sin());
    ((wave * 0.9 + shimmer * wave) * load + 0.06).min(1.0)
}

/// The heat the scene draws for a tile. Measured data wins when the chip has
/// it (the busier of the two NoCs, 0.0 for a tile the data does not mention);
/// otherwise it is the load estimate. A NaN or infinite input gives 0.0: the
/// callers turn the value into a height and a colour, and `f32::max` would
/// otherwise let a NaN in one NoC slip through as the other's value.
pub fn tile_value(chip: &ChipInputs, tile: &Tile, t: f32) -> f32 {
    match &chip.tile_flow {
        Some(flow) => flow
            .get(&tile.noc0)
            .filter(|v| v.iter().all(|x| x.is_finite()))
            .map_or(0.0, |v| v[0].max(v[1])),
        None if chip.load.is_finite() => tile_heat(chip.load, tile, t),
        None => 0.0,
    }
}

/// True when any chip carries measured per-tile data, i.e. when the estimate
/// label no longer applies to the whole picture.
pub fn has_per_tile_data(chips: &[ChipInputs]) -> bool {
    chips.iter().any(|c| c.tile_flow.is_some())
}

/// How much detail to draw.
///
/// `Blocks(n)` is for coarser pixel grids: each n x n group of tiles that holds
/// a live Tensix tile is drawn as one block, lit by the mean heat of the
/// group's live Tensix tiles (see `super_scene::block_heat`). `Blocks(0)` and
/// `Blocks(1)` are the same as `Full`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Full,
    Blocks(u8),
}

/// The projection chosen for a terminal size.
#[derive(Debug, Clone, Copy)]
pub struct SceneFit {
    pub iso: Iso,
    /// Whole-pixel unit (`tw / 8`, at least 1): the packet blob size.
    pub scale: i32,
}

/// Pixels the tallest block rises above its tile centre at tile width `tw`:
/// 13 scale units (a hot Tensix tile is 11.5 plus its 1 unit lift).
fn lift(tw: i32) -> i32 {
    (13.0 * tw as f32 / 8.0).ceil() as i32
}

/// Choose the tile size and origin for a `cells_w` x `cells_h` terminal area
/// drawn as braille (2 x 4 pixels per cell) with `dies` dies stacked on the
/// board. The largest even tile width that keeps the whole scene in the buffer
/// at every quarter turn wins, and one origin serves all four turns so the
/// camera can rotate without re-fitting. `None` below 100 x 30 cells.
///
/// `Iso` rotates about the grid centre, and the board grid is not square, so
/// each turn has its own footprint. The sizing takes the union of all four.
pub fn fit(cells_w: usize, cells_h: usize, dies: usize) -> Option<SceneFit> {
    fit_grid(cells_w, cells_h, (BOARD_COLS, BOARD_BAND * dies.max(1)))
}

/// [`fit`] for any tile grid of `grid` (columns, rows), such as the several
/// boards of the super scene.
pub(super) fn fit_grid(cells_w: usize, cells_h: usize, grid: (usize, usize)) -> Option<SceneFit> {
    if cells_w < MIN_CELLS.0 || cells_h < MIN_CELLS.1 {
        return None;
    }
    let (pw, ph) = ((cells_w * 2) as i32, (cells_h * 4) as i32);
    let corners = [
        (0.0, 0.0),
        ((grid.0 - 1) as f32, 0.0),
        (0.0, (grid.1 - 1) as f32),
        ((grid.0 - 1) as f32, (grid.1 - 1) as f32),
    ];
    for tw in (4..=64).rev().filter(|w| w % 2 == 0) {
        let th = tw / 2;
        // Union of the pixel extents over the four turns, with the origin at 0.
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for q in 0..4 {
            let iso = Iso {
                tw,
                th,
                ox: 0,
                oy: 0,
                quarter_turns: q,
            };
            for &(gx, gy) in &corners {
                let (x, y) = iso.project(gx, gy, grid);
                // A block spans cx - tw/2 ..= cx + tw/2, from `lift` above to
                // one row below its tile's base.
                x0 = x0.min(x - tw / 2);
                x1 = x1.max(x + tw / 2 + 1);
                y0 = y0.min(y - lift(tw));
                y1 = y1.max(y + th + 1);
            }
        }
        if x1 - x0 <= pw && y1 - y0 <= ph {
            return Some(SceneFit {
                iso: Iso {
                    tw,
                    th,
                    ox: (pw - (x1 - x0)) / 2 - x0,
                    oy: (ph - (y1 - y0)) / 2 - y0,
                    quarter_turns: 0,
                },
                scale: ((tw + 4) / 8).max(1),
            });
        }
    }
    None
}

/// Linear blend of two colours, rounded like the mockup's clamped byte array.
fn lerp_rgb(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let f = |i: usize| {
        (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    [f(0), f(1), f(2)]
}

/// Tile colour for a heat value: cold teal to warm yellow to hot orange.
fn heat_rgb(h: f32) -> [u8; 3] {
    if h < 0.5 {
        lerp_rgb(TILE, WARM, h * 2.0)
    } else {
        lerp_rgb(WARM, HOT, (h - 0.5) * 2.0)
    }
}

/// How many packets to draw for a PCIe rate. None for a missing, non-finite
/// or idle rate; otherwise 1..=6 on a log scale from 1 kB/s to 64 GB/s.
fn packet_count(rate: Option<f64>) -> usize {
    match rate {
        Some(r) if r.is_finite() && r > MIN_PACKET_BPS => {
            let frac = (r / MIN_PACKET_BPS).log10() / (FULL_PACKET_BPS / MIN_PACKET_BPS).log10();
            1 + (frac.clamp(0.0, 1.0) * (MAX_PACKETS - 1) as f64).round() as usize
        }
        _ => 0,
    }
}

/// A square blob of `n` x `n` pixels with its top-left corner at (`x`, `y`).
fn blob(buf: &mut PixelBuf, x: i32, y: i32, c: [u8; 3], n: i32) {
    for a in 0..n {
        for b in 0..n {
            buf.put(x + a, y + b, c);
        }
    }
}

/// What every tile of one drawing shares: the projection, the sizes derived
/// from it, the time and the detail level.
pub(super) struct DrawCtx {
    pub(super) iso: Iso,
    /// Tile width over 8: the unit the block heights are measured in.
    s: f32,
    /// Height unit for the raised blocks (faces are smooth, 15% taller).
    hs: f32,
    /// Pixels a slab is raised.
    up: i32,
    /// Packet blob size in pixels.
    n: i32,
    t: f32,
    detail: Detail,
}

impl DrawCtx {
    pub(super) fn new(fit: &SceneFit, t: f32, detail: Detail) -> DrawCtx {
        let s = fit.iso.tw as f32 / 8.0;
        DrawCtx {
            iso: fit.iso,
            s,
            hs: s * 1.15,
            up: s.round() as i32,
            n: fit.scale,
            t,
            detail,
        }
    }

    fn blocks(&self, h: f32) -> i32 {
        (h * self.hs).round() as i32
    }
}

/// One die on a board grid: its inputs, where its band starts on the grid and
/// a number that is unique among the dies of one drawing.
pub(super) struct DieSlot<'a> {
    pub(super) chip: &'a ChipInputs,
    /// Harvested die columns (see `tensix_mask_to_die_cols`).
    pub(super) harvested: &'a [u8],
    /// Grid position of the band's tile (0, 0): the first margin column of the
    /// die's cage row.
    pub(super) origin: (usize, usize),
    pub(super) slot: usize,
}

/// Harvested die columns of every chip, in chip order.
pub(super) fn harvested_columns(chips: &[ChipInputs]) -> Vec<Vec<u8>> {
    chips
        .iter()
        .map(|c| {
            c.enabled_tensix_col
                .map(tensix_mask_to_die_cols)
                .unwrap_or_default()
        })
        .collect()
}

/// Paint the checkerboard slab tile at grid position (`bx`, `by`), lighter on
/// odd (gx + gy) as in the mockup.
pub(super) fn draw_slab(
    buf: &mut PixelBuf,
    ctx: &DrawCtx,
    (bx, by): (usize, usize),
    at: (i32, i32),
) {
    let slab = if (bx + by) % 2 == 0 {
        SLAB.map(|v| (v as f32 * 1.1).round() as u8)
    } else {
        SLAB
    };
    draw_block(buf, at.0, at.1, ctx.iso.tw, ctx.iso.th, ctx.up, slab, true);
}

/// Everything that stands on one grid tile of a die band: the die tile, the
/// GDDR, PCIe and Ethernet blocks and the packets passing through. `local` is
/// the tile's position in the band (margins included), `at` its projected
/// centre and `grid` the whole grid the scene is projected on.
///
/// With `Detail::Blocks(n)` the tiles of an n x n group that holds a live
/// Tensix tile are not drawn one by one: when the group's last tile comes up in
/// back to front order the group is drawn as a single block, so everything in
/// front of it still paints over it. `visits` counts the tiles of each group
/// seen so far. A group with no live Tensix tile (Ethernet, DRAM, management
/// and harvested tiles) keeps its tiles.
pub(super) fn draw_die_tile(
    buf: &mut PixelBuf,
    ctx: &DrawCtx,
    die: &DieSlot,
    grid: (usize, usize),
    local: (usize, usize),
    at: (i32, i32),
    visits: &mut HashMap<(usize, i32, i32), usize>,
) {
    let layout = DieLayout::blackhole();
    let (cx, cy) = at;
    let (s, n, t, chip) = (ctx.s, ctx.n, ctx.t, die.chip);
    let blocks = |h: f32| ctx.blocks(h);
    let gx = local.0 as i32 - MARGIN_X as i32;
    let gy = local.1 as i32 - MARGIN_Y as i32;
    let (cy_top, th, tw) = (cy - ctx.up, ctx.iso.th, ctx.iso.tw);
    let cube =
        |buf: &mut PixelBuf, h: i32, c: [u8; 3]| draw_block(buf, cx, cy_top, tw, th, h, c, true);
    let live = |tile: &Tile| tile.kind == TileKind::Tensix && !die.harvested.contains(&tile.die.0);

    // The die itself.
    if (0..layout.cols as i32).contains(&gx) && (0..layout.rows as i32).contains(&gy) {
        if let Some(tile) = layout.tile_at((gx as u8, gy as u8)) {
            // Coarse detail: a group holding a live Tensix tile is drawn once,
            // from its last tile. Its other tiles (a stray Ethernet or
            // management tile at the die edge) are folded into the block.
            let (x0, y0, x1, y1, members) = match ctx.detail {
                Detail::Blocks(g) if g >= 2 => {
                    let g = g as i32;
                    let (x0, y0) = (gx / g * g, gy / g * g);
                    let (x1, y1) = (
                        (x0 + g).min(layout.cols as i32),
                        (y0 + g).min(layout.rows as i32),
                    );
                    let members: Vec<&Tile> = (y0..y1)
                        .flat_map(|y| (x0..x1).map(move |x| (x, y)))
                        .filter_map(|(x, y)| layout.tile_at((x as u8, y as u8)))
                        .collect();
                    (x0, y0, x1, y1, members)
                }
                _ => (0, 0, 0, 0, Vec::new()),
            };
            if members.iter().any(|m| live(m)) {
                let seen = visits.entry((die.slot, x0, y0)).or_insert(0);
                *seen += 1;
                if *seen == members.len() {
                    let heats: Vec<f32> = members
                        .iter()
                        .filter(|m| live(m))
                        .map(|m| tile_value(chip, m, t).clamp(0.0, 1.0))
                        .collect();
                    let h = mean_heat(&heats);
                    // The block stands over the middle of its group. A group cut
                    // short by the die edge is narrower or shallower, and its
                    // footprint is the average of its two sides.
                    let (k, m) = ((x1 - x0) as f32, (y1 - y0) as f32);
                    let (mx, my) = (
                        die.origin.0 as f32 + MARGIN_X as f32 + x0 as f32 + (k - 1.0) / 2.0,
                        die.origin.1 as f32 + MARGIN_Y as f32 + y0 as f32 + (m - 1.0) / 2.0,
                    );
                    let (bx, by) = ctx.iso.project(mx, my, grid);
                    let side = (k + m) / 2.0;
                    draw_block(
                        buf,
                        bx,
                        by - ctx.up,
                        (tw as f32 * side).round() as i32,
                        (th as f32 * side).round() as i32,
                        blocks(1.0 + h * 9.0),
                        heat_rgb(h),
                        true,
                    );
                }
            } else {
                match tile.kind {
                    TileKind::Tensix if die.harvested.contains(&tile.die.0) => {
                        cube(buf, blocks(1.0), TILE)
                    }
                    TileKind::Tensix => {
                        let h = tile_value(chip, tile, t).clamp(0.0, 1.0);
                        cube(buf, blocks(1.0 + h * 9.0), heat_rgb(h));
                    }
                    TileKind::Dram => {
                        let h = tile_value(chip, tile, t).clamp(0.0, 1.0);
                        cube(buf, blocks(2.0), lerp_rgb(TILE, DRAM, 0.35 + 0.4 * h));
                    }
                    // A still tint: with no per-tile data there is no activity to show.
                    TileKind::Eth => cube(buf, blocks(1.5), lerp_rgb(TILE, ETH, 0.45)),
                    TileKind::Mgmt => cube(buf, blocks(1.0), lerp_rgb(TILE, MGMT, 0.3)),
                }
            }
        }
    }
    // GDDR blocks, taller and brighter as the memory warms.
    let side = gx == -(MARGIN_X as i32) || gx == layout.cols as i32 + 1;
    if side && GDDR_ROWS.iter().any(|&r| gy >= r && gy < r + 2) {
        let heat = chip.gddr_heat.unwrap_or(0.0);
        cube(
            buf,
            blocks(2.0 + 2.0 * heat),
            lerp_rgb(TILE, DRAM, 0.35 + 0.55 * heat),
        );
    }
    // PCIe connector on the front edge.
    let front = gy == layout.rows as i32;
    if front && (6..=10).contains(&gx) {
        cube(buf, blocks(2.5), lerp_rgb(TILE, N1, 0.7));
    }
    // Ethernet cages on the back edge, lit by the live-link bits.
    if gy == -1 && (1..=ETH_CAGES as i32).contains(&gx) {
        if chip.eth_live >> (gx - 1) & 1 == 1 {
            cube(buf, blocks(2.5), lerp_rgb(TILE, ETH, 0.7));
        } else {
            cube(buf, blocks(1.0), lerp_rgb(TILE, ETH, 0.15));
        }
    }
    // Packet streams along the middle of the die, in from the connector
    // (column 8) and out toward it (column 9). Only for a real rate.
    let near = |a: f32, b: f32| (a - gx as f32).abs() < 0.5 && (b - gy as f32).abs() < 0.5;
    let dz = (4.0 * s).round() as i32;
    let span = layout.rows as f32 + 1.0;
    for k in 0..packet_count(chip.pcie_in_bps) {
        let p = layout.rows as f32 - (t * 5.0 + k as f32 * 2.3).rem_euclid(span);
        if near(8.0, p) {
            blob(buf, cx, cy - dz, WHITE, n);
            blob(buf, cx, cy - dz - n, N1, n);
        }
    }
    for k in 0..packet_count(chip.pcie_out_bps) {
        let p = (t * 5.0 + k as f32 * 2.3 + 1.1).rem_euclid(span) - 1.0;
        if near(9.0, p) {
            blob(buf, cx, cy - dz, WHITE, n);
            blob(buf, cx, cy - dz - n, PACKET_OUT, n);
        }
    }
}

/// Draw the board: a slab, one die per chip stacked along the grid y axis, the
/// GDDR blocks beside each die, the PCIe connector at the front edge and the
/// Ethernet cages at the back.
///
/// `fit` must come from `fit(.., chips.len())`. Harvested Tensix columns stay
/// flat. Blocks and packets that depend on data (GDDR, Ethernet, PCIe) are
/// drawn only from the chip's own numbers; nothing animates without a reason,
/// so an idle chip with no counters is a still picture apart from the load
/// wave. The picture is a pure function of its inputs and `t`.
pub fn draw_board_scene(
    buf: &mut PixelBuf,
    chips: &[ChipInputs],
    fit: &SceneFit,
    t: f32,
    detail: Detail,
) {
    let ctx = DrawCtx::new(fit, t, detail);
    let grid = (BOARD_COLS, BOARD_BAND * chips.len().max(1));
    buf.fill_vignette(BG);
    let harvested = harvested_columns(chips);
    let mut visits = HashMap::new();
    for (bx, by) in ctx.iso.draw_order(grid) {
        let at = ctx.iso.project(bx as f32, by as f32, grid);
        draw_slab(buf, &ctx, (bx, by), at);
        let die = by / BOARD_BAND;
        let Some(chip) = chips.get(die) else { continue };
        let slot = DieSlot {
            chip,
            harvested: &harvested[die],
            origin: (0, die * BOARD_BAND),
            slot: die,
        };
        draw_die_tile(
            buf,
            &ctx,
            &slot,
            grid,
            (bx, by % BOARD_BAND),
            at,
            &mut visits,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::pcie_counters::PcieBandwidth;
    use crate::backend::TelemetryBackend;
    use crate::models::{Architecture, Device, SmbusTelemetry, Telemetry};

    /// A backend whose power, TDP and PCIe rate are set directly, so the tests
    /// do not depend on the mock backend's simulated load.
    struct Fake {
        devices: Vec<Device>,
        telem: Telemetry,
        smbus: SmbusTelemetry,
        has_smbus: bool,
        pcie: Option<PcieBandwidth>,
    }

    impl Fake {
        fn new(power: f32, smbus_tdp: Option<&str>, limit_tdp: Option<f32>) -> Fake {
            let mut d = Device::new(0, "p300".into(), "0000:01:00.0".into(), "(0,0)".into());
            d.limits = limit_tdp.map(|t| crate::models::telemetry::DeviceLimits {
                tdp_limit: Some(t),
                tdc_limit: None,
                asic_fmax: None,
                thm_limit: None,
            });
            let mut telem = Telemetry::new();
            telem.power = Some(power);
            let mut smbus = SmbusTelemetry::new();
            smbus.tdp = smbus_tdp.map(str::to_string);
            Fake {
                devices: vec![d],
                telem,
                smbus,
                has_smbus: true,
                pcie: None,
            }
        }
    }

    impl TelemetryBackend for Fake {
        fn init(&mut self) -> crate::error::BackendResult<()> {
            Ok(())
        }
        fn update(&mut self) -> crate::error::BackendResult<()> {
            Ok(())
        }
        fn devices(&self) -> &[Device] {
            &self.devices
        }
        fn telemetry(&self, _i: usize) -> Option<&Telemetry> {
            Some(&self.telem)
        }
        fn smbus_telemetry(&self, _i: usize) -> Option<&SmbusTelemetry> {
            self.has_smbus.then_some(&self.smbus)
        }
        fn backend_info(&self) -> String {
            "fake".into()
        }
        fn pcie_bandwidth(&self, _i: usize) -> Option<PcieBandwidth> {
            self.pcie
        }
    }

    fn chip(load: f32) -> ChipInputs {
        ChipInputs {
            index: 0,
            arch: Architecture::Blackhole,
            load,
            pcie_in_bps: None,
            pcie_out_bps: None,
            gddr_heat: Some(0.0),
            eth_live: 0,
            temp_c: None,
            enabled_tensix_col: None,
            tile_flow: None,
        }
    }

    fn tensix_tiles() -> Vec<Tile> {
        DieLayout::blackhole()
            .tiles
            .iter()
            .copied()
            .filter(|t| t.kind == TileKind::Tensix)
            .collect()
    }

    /// Render one board scene at the 150 x 42 cell reference size.
    fn render(chips: &[ChipInputs], turn: u8, t: f32) -> PixelBuf {
        let mut fit = fit(150, 42, chips.len()).expect("150 x 42 fits");
        fit.iso.quarter_turns = turn;
        let mut buf = PixelBuf::new(300, 168);
        draw_board_scene(&mut buf, chips, &fit, t, Detail::Full);
        buf
    }

    #[test]
    fn load_is_power_over_tdp_clamped() {
        // 300 W against a 150 W SMBUS TDP is twice the TDP: clamps to 1.0.
        let b = Fake::new(300.0, Some("150"), None);
        assert_eq!(chip_inputs(&b, &b.devices[0]).load, 1.0);
        // Half of the TDP.
        let b = Fake::new(75.0, Some("150"), None);
        assert!((chip_inputs(&b, &b.devices[0]).load - 0.5).abs() < 1e-6);
        // No SMBUS TDP: the device limits block supplies it.
        let b = Fake::new(75.0, None, Some(300.0));
        assert!((chip_inputs(&b, &b.devices[0]).load - 0.25).abs() < 1e-6);
        // No TDP anywhere: unknown is 0.0, not a divide by zero.
        let mut b = Fake::new(300.0, None, None);
        b.has_smbus = false;
        assert_eq!(chip_inputs(&b, &b.devices[0]).load, 0.0);
        // Per-tile data is never filled at tier 0.
        assert!(chip_inputs(&b, &b.devices[0]).tile_flow.is_none());
    }

    #[test]
    fn chip_inputs_reads_pcie_eth_harvest_and_gddr() {
        let mut b = Fake::new(10.0, Some("150"), None);
        b.pcie = Some(PcieBandwidth {
            rx_bytes_per_sec: 2.0e9,
            tx_bytes_per_sec: 1.0e6,
        });
        b.smbus.eth_live_status = Some(0b1011);
        b.smbus.enabled_tensix_col = Some(0x3edf);
        b.smbus.gddr_temps[0] = Some(crate::models::telemetry::GddrTempPair([
            65.0, 65.0, 65.0, 65.0,
        ]));
        let c = chip_inputs(&b, &b.devices[0]);
        assert_eq!(c.pcie_in_bps, Some(2.0e9));
        assert_eq!(c.pcie_out_bps, Some(1.0e6));
        assert_eq!(c.eth_live, 0b1011);
        assert_eq!(c.enabled_tensix_col, Some(0x3edf));
        // (65 - 40) / 50 = 0.5
        assert!((c.gddr_heat.unwrap() - 0.5).abs() < 1e-6);
        // Without SMBUS, eth is 0 and the rest is absent.
        b.has_smbus = false;
        let c = chip_inputs(&b, &b.devices[0]);
        assert_eq!(
            (c.eth_live, c.gddr_heat, c.enabled_tensix_col),
            (0, None, None)
        );
    }

    #[test]
    fn heat_scales_with_load() {
        let tiles = tensix_tiles();
        let mut best = 0.0f32;
        for step in 0..400 {
            let t = step as f32 * 0.05;
            for tile in &tiles {
                assert!(tile_heat(0.0, tile, t) <= 0.1, "load 0 must stay cold");
                best = best.max(tile_heat(1.0, tile, t));
            }
        }
        assert!(best >= 0.8, "load 1 peaked at {best}");
    }

    #[test]
    fn non_tensix_tiles_have_no_wave() {
        let l = DieLayout::blackhole();
        for kind in [TileKind::Dram, TileKind::Eth] {
            let tile = l.tiles.iter().find(|t| t.kind == kind).unwrap();
            for step in 0..40 {
                assert_eq!(tile_heat(1.0, tile, step as f32 * 0.5), 0.0);
            }
        }
    }

    #[test]
    fn fit_returns_none_below_100_by_30() {
        assert!(fit(99, 42, 1).is_none());
        assert!(fit(150, 29, 1).is_none());
        assert!(fit(100, 30, 1).is_some());
    }

    #[test]
    fn fit_at_150_by_42_matches_the_mockup_scale() {
        let f = fit(150, 42, 1).unwrap();
        assert_eq!((f.iso.tw, f.iso.th, f.scale), (16, 8, 2));
    }

    #[test]
    fn two_dies_get_a_smaller_tile_than_one() {
        let one = fit(150, 42, 1).unwrap();
        let two = fit(150, 42, 2).unwrap();
        assert!(two.iso.tw < one.iso.tw);
    }

    /// The bounding box `(x0, y0, x1, y1)` (exclusive end) of everything the
    /// scene painted, found by drawing into a buffer with `PAD` spare pixels on
    /// every side. `PixelBuf::put` drops out-of-range pixels silently, so only
    /// a larger buffer can show what a scene would have clipped.
    fn painted_bbox(
        chips: &[ChipInputs],
        cells: (usize, usize),
        turn: u8,
    ) -> (usize, usize, usize, usize) {
        const PAD: usize = 80;
        let mut f = fit(cells.0, cells.1, chips.len()).unwrap();
        f.iso.quarter_turns = turn;
        f.iso.ox += PAD as i32;
        f.iso.oy += PAD as i32;
        let (w, h) = (cells.0 * 2 + 2 * PAD, cells.1 * 4 + 2 * PAD);
        let mut drawn = PixelBuf::new(w, h);
        draw_board_scene(&mut drawn, chips, &f, 1.3, Detail::Full);
        let mut blank = PixelBuf::new(w, h);
        blank.fill_vignette(BG);
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for y in 0..h {
            for x in 0..w {
                if drawn.get(x, y) != blank.get(x, y) {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x + 1);
                    y1 = y1.max(y + 1);
                }
            }
        }
        (x0 - PAD, y0 - PAD, x1 - PAD, y1 - PAD)
    }

    fn busy_chip() -> ChipInputs {
        let mut c = chip(1.0);
        c.gddr_heat = Some(1.0);
        c.eth_live = u64::MAX;
        c.pcie_in_bps = Some(40.0e9);
        c.pcie_out_bps = Some(40.0e9);
        c
    }

    #[test]
    fn two_die_board_fits_the_reference_buffer() {
        // Every quarter turn, one and two dies: the rotated footprint of the
        // non-square grid must stay inside the 300 x 168 buffer.
        for dies in [1usize, 2] {
            let chips: Vec<ChipInputs> = (0..dies).map(|_| busy_chip()).collect();
            for turn in 0..4 {
                let (x0, y0, x1, y1) = painted_bbox(&chips, (150, 42), turn);
                assert!(
                    x1 <= 300 && y1 <= 168,
                    "dies={dies} turn={turn}: {x0},{y0}..{x1},{y1}"
                );
                // Not vacuous: the scene fills most of the buffer.
                assert!(
                    x1 - x0 > 200 && y1 - y0 > 100,
                    "dies={dies} turn={turn}: {x0},{y0}..{x1},{y1}"
                );
            }
        }
    }

    #[test]
    fn the_scene_fits_the_smallest_terminal_at_every_turn() {
        for dies in [1usize, 2] {
            let chips: Vec<ChipInputs> = (0..dies).map(|_| busy_chip()).collect();
            for turn in 0..4 {
                let (_, _, x1, y1) = painted_bbox(&chips, (100, 30), turn);
                assert!(x1 <= 200 && y1 <= 120, "dies={dies} turn={turn}: {x1},{y1}");
            }
        }
    }

    #[test]
    fn no_packets_for_a_missing_or_non_finite_pcie_rate() {
        let mut base = chip(0.5);
        base.pcie_in_bps = None;
        base.pcie_out_bps = None;
        let want = render(&[base.clone()], 0, 2.0);
        for bad in [
            Some(f64::NAN),
            Some(f64::INFINITY),
            Some(f64::NEG_INFINITY),
            Some(500.0),
            Some(0.0),
        ] {
            let mut c = base.clone();
            c.pcie_in_bps = bad;
            c.pcie_out_bps = bad;
            assert_eq!(render(&[c], 0, 2.0).rgb, want.rgb, "{bad:?}");
        }
        // Control: a real rate does draw packets, so the comparison can fail.
        let mut c = base.clone();
        c.pcie_in_bps = Some(1.0e9);
        assert_ne!(render(&[c], 0, 2.0).rgb, want.rgb);
        let mut c = base;
        c.pcie_out_bps = Some(1.0e9);
        assert_ne!(render(&[c], 0, 2.0).rgb, want.rgb);
    }

    #[test]
    fn drawing_is_deterministic_for_a_given_time() {
        let c = busy_chip();
        assert_eq!(render(&[c.clone()], 1, 3.7).rgb, render(&[c], 1, 3.7).rgb);
    }

    #[test]
    fn harvested_columns_are_drawn_flat() {
        // Mask 0 harvests every Tensix column. The chip's load then changes
        // nothing on the die (the GDDR, Ethernet and PCIe inputs are held
        // fixed), so the two draws are identical at any time.
        let mut cold = chip(0.0);
        let mut hot = chip(1.0);
        cold.enabled_tensix_col = Some(0);
        hot.enabled_tensix_col = Some(0);
        for t in [0.0, 4.0, 9.0] {
            assert_eq!(
                render(&[cold.clone()], 0, t).rgb,
                render(&[hot.clone()], 0, t).rgb
            );
        }
        // Control: with no column harvested the same load change is visible.
        cold.enabled_tensix_col = Some(0x3fff);
        hot.enabled_tensix_col = Some(0x3fff);
        assert_ne!(render(&[cold], 0, 4.0).rgb, render(&[hot], 0, 4.0).rgb);
    }

    #[test]
    fn per_tile_flow_overrides_the_estimate() {
        let tiles = tensix_tiles();
        let hot = *tiles.iter().find(|t| t.die == (2, 5)).unwrap();
        let flat = *tiles.iter().find(|t| t.die == (7, 5)).unwrap();
        let mut c = chip(0.0);
        let mut flow = HashMap::new();
        flow.insert(hot.noc0, [1.0f32, 0.25]);
        c.tile_flow = Some(flow);
        // The value comes from the map: the larger NoC, 0.0 when absent.
        assert_eq!(tile_value(&c, &hot, 0.0), 1.0);
        assert_eq!(tile_value(&c, &flat, 0.0), 0.0);
        // And it is what the scene draws: the hot tile's top face is the hot
        // colour, a neighbour's is the cold tile colour.
        let f = fit(150, 42, 1).unwrap();
        let buf = render(&[c.clone()], 0, 1.0);
        let grid = (BOARD_COLS, BOARD_BAND);
        let s = f.iso.tw as f32 / 8.0;
        let up = s.round() as i32;
        let probe = |tile: &Tile, height: f32| {
            let (cx, cy) = f.iso.project(
                (tile.die.0 as usize + MARGIN_X) as f32,
                (tile.die.1 as usize + MARGIN_Y) as f32,
                grid,
            );
            buf.get(
                cx as usize,
                (cy - up - height.round() as i32 + f.iso.th / 2) as usize,
            )
        };
        let hs = s * 1.15;
        assert_eq!(probe(&hot, hs + 9.0 * hs), HOT);
        assert_eq!(probe(&flat, hs), TILE);
        // Without the flow the same load 0.0 chip draws no hot tile at all.
        c.tile_flow = None;
        let cold = render(&[c], 0, 1.0);
        assert_ne!(cold.rgb, buf.rgb);
    }

    #[test]
    fn has_per_tile_data_is_false_when_every_flow_is_none() {
        let a = chip(0.3);
        let mut b = chip(0.3);
        assert!(!has_per_tile_data(&[a.clone(), b.clone()]));
        assert!(!has_per_tile_data(&[]));
        b.tile_flow = Some(HashMap::new());
        assert!(has_per_tile_data(&[a, b]));
    }

    #[test]
    fn tile_value_is_zero_for_non_finite_inputs() {
        let tiles = tensix_tiles();
        let tile = *tiles.iter().find(|t| t.die == (2, 5)).unwrap();
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            // The estimate: a bad load is no load.
            assert_eq!(tile_value(&chip(bad), &tile, 1.0), 0.0, "load {bad}");
            // Measured: a bad value on either NoC voids the tile, even when the
            // other NoC is fine (`f32::max` alone would return that one).
            for v in [[bad, 0.25], [0.25, bad], [bad, bad]] {
                let mut c = chip(0.5);
                c.tile_flow = Some(HashMap::from([(tile.noc0, v)]));
                assert_eq!(tile_value(&c, &tile, 1.0), 0.0, "flow {v:?}");
            }
        }
        // Control: finite inputs still give their value.
        let mut c = chip(0.5);
        c.tile_flow = Some(HashMap::from([(tile.noc0, [0.25f32, 0.5])]));
        assert_eq!(tile_value(&c, &tile, 1.0), 0.5);
        assert!(tile_value(&chip(1.0), &tile, 1.0) > 0.0);
    }

    #[test]
    fn blocks_detail_draws_a_group_as_one_block_lit_by_its_mean() {
        let layout = DieLayout::blackhole();
        // Die tiles (2,4) (3,4) (2,5) (3,5): one group of four live Tensix tiles.
        let group: Vec<Tile> = [(2u8, 4u8), (3, 4), (2, 5), (3, 5)]
            .iter()
            .map(|&d| *layout.tile_at(d).unwrap())
            .collect();
        assert!(group.iter().all(|t| t.kind == TileKind::Tensix));
        // Only one of the four is hot: the block is lit at the mean, 0.25.
        let mut c = chip(0.0);
        c.tile_flow = Some(HashMap::from([(group[0].noc0, [1.0f32, 0.0])]));
        let f = fit(150, 42, 1).unwrap();
        let grid = (BOARD_COLS, BOARD_BAND);
        let mut blocks = PixelBuf::new(300, 168);
        draw_board_scene(&mut blocks, &[c.clone()], &f, 1.0, Detail::Blocks(2));
        let mut full = PixelBuf::new(300, 168);
        draw_board_scene(&mut full, &[c], &f, 1.0, Detail::Full);
        assert_ne!(blocks.rgb, full.rgb);

        let s = f.iso.tw as f32 / 8.0;
        let up = s.round() as i32;
        let height = ((1.0 + 0.25 * 9.0) * s * 1.15).round() as i32;
        let (cx, cy) = f.iso.project(
            (2 + MARGIN_X) as f32 + 0.5,
            (4 + MARGIN_Y) as f32 + 0.5,
            grid,
        );
        let top = (cy - up - height + f.iso.th) as usize;
        // The middle of the block's top face is the mean colour ...
        assert_eq!(blocks.get(cx as usize, top), heat_rgb(0.25));
        // ... where tile by tile there is the seam between four tiles, which
        // the hot tile's own colour does not fill.
        assert_ne!(full.get(cx as usize, top), heat_rgb(0.25));
        // Blocks(1) is Full.
        let mut one = PixelBuf::new(300, 168);
        let mut c = chip(0.0);
        c.tile_flow = Some(HashMap::from([(group[0].noc0, [1.0f32, 0.0])]));
        draw_board_scene(&mut one, &[c], &f, 1.0, Detail::Blocks(1));
        assert_eq!(one.rgb, full.rgb);
    }
}
