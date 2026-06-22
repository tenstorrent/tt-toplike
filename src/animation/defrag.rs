// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Defrag View — model-loading visualization
//!
//! Inspired by Norton SpeedDisk / DOS Defrag: a grid of memory blocks per device.
//! Each row = one GDDR channel (0–7).  Blocks fill left→right as weights DMA in,
//! then glow while inference runs, then fade when idle.
//!
//! ## Signals and their meanings
//!
//! | Signal           | Meaning in view                                  |
//! |------------------|--------------------------------------------------|
//! | aiclk            | Phase: 0 = DMA loading, high = executing         |
//! | DDR_STATUS nibble| Per-channel init state (trained / untrained)     |
//! | GDDR_x_y_TEMP    | Per-channel heat → color saturation & shimmer    |
//! | PCIe usage       | DMA bandwidth → write-head sweep speed           |
//! | ETH_LIVE_STATUS  | Inter-chip links → link-pulse animation          |
//! | ENABLED_GDDR     | Which channels exist on this chip                |
//! | heartbeat        | ARC alive → periodic global pulse                |
//! | corr_errs        | GDDR correctable errors → glitch flash           |
//! | power (TDP)      | Overall load → brightness                        |
//!
//! ## Block states
//!
//! ```
//! ░  empty / uninitialized   (very dark)
//! ▒  DDR channel trained, ready for data  (dim)
//! ▓  write frontier (DMA in progress)     (bright, animated)
//! █  resident / written                   (full, colored by temp)
//! ·  seek ahead of frontier               (faint dot)
//! ✗  correctable error flash              (brief red, then heals)
//! ```
//!
//! ## Phases
//!
//! 1. **INIT** — DDR not yet trained: cells show `░`, slowly cycling `▒` as
//!    channels train one by one (DDR_STATUS nibble goes 0→5).
//! 2. **DMA** — aiclk=0, PCIe active: write head sweeps left→right per channel,
//!    speed driven by PCIe usage.  Channels with higher GDDR temp fill faster.
//! 3. **RUNNING** — aiclk high: blocks glow with heat shimmer, ETH links pulse,
//!    power drives brightness.  No new blocks being written.
//! 4. **IDLE** — power low: blocks slowly drain back to empty.

use crate::animation::{hsv_to_rgb, temp_to_hue};
use crate::backend::TelemetryBackend;
use crate::models::telemetry::parse_hex_or_dec;
use crate::ui::colors;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

const GDDR_CHANNELS: usize = 8;
const POWER_IDLE_W: f32 = 8.0;

// EVICT is triggered by real hardware events, not a frame timer.
// See Phase transition logic in update().

/// Which phase this device is in.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    /// DDR channels still initializing / training.
    Init,
    /// DMA active: weights copying from host.
    Dma,
    /// Model is executing on Tensix.
    Running,
    /// Idle draining after activity stops.
    Idle,
    /// Theatrical deconstruct: blocks evaporate right→left per channel,
    /// then immediately trigger a rebuild (Dma animation from scratch).
    Deconstructing,
}

/// Per-channel state.
#[derive(Debug, Clone)]
struct Channel {
    /// DDR training status nibble for this channel-pair (0=untrained, 5=trained).
    trained: bool,
    /// Fill fraction [0, 1] — how much of the channel's GDDR capacity is loaded.
    fill: f32,
    /// Smoothed temperature (EMA).
    temp_ema: f32,
    /// Correctable error count (last seen).
    last_corr_errs: u32,
    /// Frames remaining to show an error flash.
    err_flash: u8,
    /// Whether this channel is physically enabled on this chip.
    enabled: bool,
    /// Write-head position (0..=1) during DMA — trails behind `fill`.
    head_pos: f32,
}

impl Channel {
    fn new() -> Self {
        Self {
            trained: false,
            fill: 0.0,
            temp_ema: 25.0,
            last_corr_errs: 0,
            err_flash: 0,
            enabled: true,
            head_pos: 0.0,
        }
    }
}

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

/// Per-device state.
#[derive(Debug, Clone)]
struct DeviceState {
    channels: [Channel; GDDR_CHANNELS],
    phase: Phase,
    /// Fast-smoothed power (responds quickly to spikes).
    power_ema: f32,
    /// Slow-smoothed power (long-term baseline; used to detect surges).
    power_ema_slow: f32,
    /// Baseline power when last in idle.
    idle_power: f32,
    /// Frame phase entered, for per-channel stagger.
    dma_start_frame: Option<u64>,
    /// PCIe usage (0–4 = link width in units of x4).
    pcie_usage: u32,
    /// Eth link count (from ETH_LIVE_STATUS popcount).
    eth_links: u32,
    /// Whether ARC heartbeat has changed recently.
    heartbeat_pulse: u8,
    last_heartbeat: u32,
    /// Frame at which we entered Running phase — used for surge-EVICT cooldown.
    running_since: Option<u64>,
    /// Cooldown counter after an EVICT — prevents immediate re-trigger on refill.
    evict_cooldown: u64,
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
}

impl DeviceState {
    fn new(_device_idx: usize) -> Self {
        Self {
            channels: std::array::from_fn(|_| Channel::new()),
            phase: Phase::Idle,
            power_ema: 0.0,
            power_ema_slow: 0.0,
            idle_power: 0.0,
            dma_start_frame: None,
            pcie_usage: 0,
            eth_links: 0,
            heartbeat_pulse: 0,
            last_heartbeat: 0,
            running_since: None,
            evict_cooldown: 0,
            thermal_mood: 0.0,
            inference_energy: 0.0,
            scatter_bursts: Vec::new(),
            burst_cooldown: 0,
        }
    }

    fn mean_fill(&self) -> f32 {
        let enabled: Vec<_> = self.channels.iter().filter(|c| c.enabled).collect();
        if enabled.is_empty() { return 0.0; }
        enabled.iter().map(|c| c.fill).sum::<f32>() / enabled.len() as f32
    }
}

/// Defrag visualization.
pub struct DefragVis {
    width: usize,
    height: usize,
    frame: u64,
    devices: std::collections::HashMap<usize, DeviceState>,
}

impl DefragVis {
    pub fn new(width: usize, height: usize) -> Self {
        Self { width, height, frame: 0, devices: Default::default() }
    }

    pub fn update(&mut self, backend: &dyn TelemetryBackend) {
        self.frame = self.frame.wrapping_add(1);

        for device in backend.devices() {
            let idx = device.index;
            let ds = self.devices.entry(idx).or_insert_with(|| DeviceState::new(idx));

            let telem = backend.telemetry(idx);
            let smbus = backend.smbus_telemetry(idx);

            let power  = telem.map(|t| t.power_w()).unwrap_or(0.0);
            // Prefer sysfs aiclk; fall back to SMBUS AICLK register (hex string).
            // Sysfs reads 0 during GDDR DMA even when Tensix is running, so SMBUS
            // is the authoritative source.
            let smbus_aiclk: u32 = smbus
                .and_then(|s| s.aiclk.as_deref())
                .and_then(|v| parse_hex_or_dec(v))
                .unwrap_or(0);
            let sysfs_aiclk = telem.and_then(|t| t.aiclk).unwrap_or(0);
            let aiclk = smbus_aiclk.max(sysfs_aiclk);
            let hb     = telem.and_then(|t| t.heartbeat).unwrap_or(0);

            // Fast EMA (noise filter for phase transitions).
            ds.power_ema = ds.power_ema * 0.92 + power * 0.08;
            // Slow EMA (long-term baseline; surge = fast >> slow).
            ds.power_ema_slow = ds.power_ema_slow * 0.992 + power * 0.008;
            if ds.evict_cooldown > 0 { ds.evict_cooldown -= 1; }

            // Heartbeat pulse
            if hb != ds.last_heartbeat && hb != 0 {
                ds.heartbeat_pulse = 8;
                ds.last_heartbeat = hb;
            }
            if ds.heartbeat_pulse > 0 { ds.heartbeat_pulse -= 1; }

            // Per-channel temps from SMBUS
            let ch_temps: [f32; GDDR_CHANNELS] = if let Some(s) = smbus {
                [
                    s.gddr_temps[0].map(|p| p.0[0]).unwrap_or(25.0),
                    s.gddr_temps[0].map(|p| p.0[1]).unwrap_or(25.0),
                    s.gddr_temps[1].map(|p| p.0[0]).unwrap_or(25.0),
                    s.gddr_temps[1].map(|p| p.0[1]).unwrap_or(25.0),
                    s.gddr_temps[2].map(|p| p.0[0]).unwrap_or(25.0),
                    s.gddr_temps[2].map(|p| p.0[1]).unwrap_or(25.0),
                    s.gddr_temps[3].map(|p| p.0[0]).unwrap_or(25.0),
                    s.gddr_temps[3].map(|p| p.0[1]).unwrap_or(25.0),
                ]
            } else { [25.0; GDDR_CHANNELS] };

            // Per-channel DDR training status (nibble per channel-pair from DDR_STATUS)
            let ddr_mask: u64 = smbus
                .and_then(|s| s.ddr_status_bitmask())
                .unwrap_or(0xffff_ffff_ffff_ffff); // assume trained if no smbus

            // ENABLED_GDDR bitmask — one bit per channel
            let enabled_mask: u8 = smbus
                .and_then(|s| s.enabled_gddr)
                .map(|v| v as u8)
                .unwrap_or(0xff);

            // PCIe usage (raw field: x4/x8/x16 encoded as 4/8/16)
            ds.pcie_usage = smbus
                .and_then(|s| s.pcie_status.as_deref())
                .and_then(|v| parse_hex_or_dec(v))
                .unwrap_or(0);

            // ETH live link count
            ds.eth_links = smbus
                .and_then(|s| s.eth_live_status)
                .map(|v| v.count_ones())
                .unwrap_or(0);

            // Per-channel correctable errors (one counter per pair)
            let corr: [u32; 4] = if let Some(s) = smbus {
                [
                    s.gddr_corr_errs[0].unwrap_or(0) as u32,
                    s.gddr_corr_errs[1].unwrap_or(0) as u32,
                    s.gddr_corr_errs[2].unwrap_or(0) as u32,
                    s.gddr_corr_errs[3].unwrap_or(0) as u32,
                ]
            } else { [0; 4] };

            // Update per-channel static properties
            for (ch_idx, ch) in ds.channels.iter_mut().enumerate() {
                ch.enabled = (enabled_mask >> ch_idx) & 1 == 1;
                ch.temp_ema = ch.temp_ema * 0.92 + ch_temps[ch_idx] * 0.08;

                // DDR_STATUS: 4 bits per channel-pair; nibble 0 = channels 0-1, etc.
                // Bit pattern 0x5 = trained for a pair.
                let nibble_idx = ch_idx / 2;
                let nibble = ((ddr_mask >> (nibble_idx * 4)) & 0xf) as u8;
                ch.trained = nibble >= 4; // 4=init, 5=trained

                // Error flash detection
                let pair_errs = corr[ch_idx / 2];
                if pair_errs > ch.last_corr_errs {
                    ch.err_flash = 12; // flash for ~12 frames
                }
                ch.last_corr_errs = pair_errs;
                if ch.err_flash > 0 { ch.err_flash -= 1; }
            }

            // All-channels-full → force Running even if aiclk still reads 0.
            let all_full = ds.channels.iter()
                .filter(|c| c.enabled && c.trained)
                .all(|c| c.fill >= 0.999);

            // Phase detection.
            //
            // EVICT fires on two conditions:
            //   1. Real hardware: power drops below idle threshold (model unloaded).
            //   2. Inference surge: fast EMA spikes well above slow baseline, matching
            //      the signal that causes Memory Flow to flash color blocks.  This lets
            //      the two visualizations react to the same inference event together.
            //      A cooldown prevents re-triggering until DMA refill completes.
            //
            // SURGE_RATIO: fast EMA must be this much above slow EMA to count.
            // 1.12 = 12% above baseline, tuned to match Memory Flow's power_change
            // threshold without false-firing on gentle power ramps.
            const SURGE_RATIO: f32 = 1.12;
            // Minimum frames in Running before surge-EVICT is eligible (lets DMA
            // refill complete and channels stabilize after the previous EVICT).
            const SURGE_MIN_RUNNING_FRAMES: u64 = 600; // ~10 s
            let running_frames = ds.running_since
                .map(|s| self.frame.saturating_sub(s))
                .unwrap_or(0);
            let surge_evict = ds.phase == Phase::Running
                && ds.evict_cooldown == 0
                && running_frames >= SURGE_MIN_RUNNING_FRAMES
                && ds.power_ema_slow > 5.0 // slow EMA must be initialized
                && ds.power_ema >= ds.power_ema_slow * SURGE_RATIO;

            let new_phase = if ds.phase == Phase::Deconstructing {
                let all_empty = ds.channels.iter().all(|c| c.fill <= 0.0);
                if all_empty { Phase::Dma } else { Phase::Deconstructing }
            } else if !ds.channels.iter().any(|c| c.enabled && c.trained) {
                Phase::Init
            } else if ds.phase == Phase::Running && (power <= POWER_IDLE_W || surge_evict) {
                // Power dropped (model unloaded) OR inference surge → EVICT.
                Phase::Deconstructing
            } else if (aiclk >= 200 || all_full) && power > POWER_IDLE_W {
                Phase::Running
            } else if aiclk < 200 && power > POWER_IDLE_W {
                Phase::Dma
            } else {
                Phase::Idle
            };

            // Transition side-effects
            match (ds.phase, new_phase) {
                (p, Phase::Dma) if p != Phase::Dma => {
                    ds.dma_start_frame = Some(self.frame);
                    ds.running_since = None;
                    if p == Phase::Deconstructing {
                        for ch in ds.channels.iter_mut() {
                            ch.fill = 0.0;
                            ch.head_pos = 0.0;
                        }
                    } else {
                        for ch in ds.channels.iter_mut() { ch.head_pos = ch.fill; }
                    }
                    ds.idle_power = ds.power_ema;
                }
                (p, Phase::Running) if p != Phase::Running => {
                    ds.running_since = Some(self.frame);
                    for ch in ds.channels.iter_mut() {
                        if ch.enabled && ch.trained { ch.fill = 1.0; ch.head_pos = 1.0; }
                    }
                }
                (_, Phase::Deconstructing) => {
                    ds.running_since = None;
                    // Cooldown: ~45 s — long enough to cover full EVICT + DMA refill
                    // so the surge trigger doesn't fire again immediately on refill.
                    ds.evict_cooldown = 2700;
                }
                (_, Phase::Idle) => { ds.idle_power = ds.power_ema; }
                _ => {}
            }

            ds.phase = new_phase;

            // Per-frame updates
            match ds.phase {
                Phase::Init => {}

                Phase::Dma => {
                    let start = ds.dma_start_frame.unwrap_or(self.frame);
                    // PCIe link width drives fill speed.  Without SMBUS (mock / sysfs),
                    // pcie_usage=0 → use mid speed so DMA is clearly visible.
                    let pcie_speed = if ds.pcie_usage >= 16 { 1.0_f32 }
                        else if ds.pcie_usage >= 8 { 0.65 }
                        else if ds.pcie_usage >= 4 { 0.45 }
                        else { 0.35 }; // slower mock default — DMA sweep is the show
                    for (ch_idx, ch) in ds.channels.iter_mut().enumerate() {
                        if !ch.enabled || !ch.trained { continue; }
                        // Stagger channel starts so they don't all begin simultaneously.
                        // Channels stagger by 20 frames each (was 8) for clear cascade.
                        let stagger = ch_idx as u64 * 20;
                        if self.frame < start + stagger { continue; }
                        let heat = ((ch.temp_ema - 25.0) / 20.0).clamp(0.0, 1.5);
                        // Base rate 0.0012 → ~14 s full fill at mock speed. Theatrical.
                        let rate = 0.0012 * pcie_speed * (1.0 + heat * 0.4);
                        ch.fill = (ch.fill + rate).min(1.0);
                        ch.head_pos = ch.fill;
                    }
                }

                Phase::Running => {
                    // No fill change — shimmer is purely in render().
                }

                Phase::Idle => {
                    for ch in ds.channels.iter_mut() {
                        ch.fill = (ch.fill - 0.0015).max(0.0);
                        ch.head_pos = ch.fill;
                    }
                }

                Phase::Deconstructing => {
                    // Blocks evaporate right→left per channel at staggered rates.
                    // Channel 0 drains fastest (first in, first out), channel 7 slowest.
                    // Prime-modulated scatter makes each channel's edge feel alive.
                    for (ch_idx, ch) in ds.channels.iter_mut().enumerate() {
                        if !ch.enabled { continue; }
                        let primes = [7u64, 11, 13, 17, 19, 23, 29, 31];
                        let p = primes[ch_idx];
                        // Channel 0 drains ~2.5× faster than channel 7.
                        // Slower overall (0.003 base vs 0.006) — makes the show last ~8 s.
                        let speed_mult = 1.0 + (GDDR_CHANNELS - 1 - ch_idx) as f32 * 0.2;
                        let drain = 0.003 * speed_mult;
                        ch.fill = (ch.fill - drain).max(0.0);
                        ch.head_pos = ch.fill;
                        // Scatter: edge jumps ahead momentarily on prime-beat frames
                        if self.frame % p == 0 && ch.fill > 0.05 {
                            ch.head_pos = (ch.fill + 0.10).min(1.0);
                        }
                    }
                }
            }
        }
    }

    pub fn render(&self, backend: &dyn TelemetryBackend) -> Vec<Line<'static>> {
        let mut lines = self.render_inner(backend);
        lines.push(Line::from(""));
        lines.push(self.render_footer());
        lines
    }

    /// Render without header blank line, footer, or hotkey bar — for embedding
    /// inside arcade's split panel where the global footer is already shown.
    pub fn render_embedded(&self, backend: &dyn TelemetryBackend) -> Vec<Line<'static>> {
        self.render_inner(backend)
    }

    fn render_inner(&self, backend: &dyn TelemetryBackend) -> Vec<Line<'static>> {
        let devices = backend.devices();
        if devices.is_empty() {
            return vec![Line::from("  No devices detected.")];
        }

        let mut lines: Vec<Line<'static>> = Vec::with_capacity(self.height);
        lines.push(self.render_header(backend));
        lines.push(Line::from(""));

        let content_h = self.height.saturating_sub(4);
        let n = devices.len();
        let gutter = 1_usize;
        let avail_w = self.width.saturating_sub(gutter * n.saturating_sub(1));
        let grid_w = (avail_w / n).max(4);
        let header_rows = 2; // label + power bar
        let ch_rows_total = content_h.saturating_sub(header_rows);
        let rows_per_channel = (ch_rows_total / GDDR_CHANNELS).max(1);

        for row in 0..content_h {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (d_idx, device) in devices.iter().enumerate() {
                let idx = device.index;
                let telem  = backend.telemetry(idx);
                let ds     = self.devices.get(&idx);

                let power  = telem.map(|t| t.power_w()).unwrap_or(0.0);
                let temp   = telem.map(|t| t.temp_c()).unwrap_or(25.0);
                let smbus  = backend.smbus_telemetry(idx);
                let smbus_aiclk: u32 = smbus
                    .and_then(|s| s.aiclk.as_deref())
                    .and_then(|v| parse_hex_or_dec(v))
                    .unwrap_or(0);
                let aiclk = smbus_aiclk.max(telem.and_then(|t| t.aiclk).unwrap_or(0));

                // mvddq_power: memory-subsystem watts (GDDR + I/O), separate from Tensix.
                // 0.0 when unavailable (sysfs path, older FW).  Normalised to [0,1]
                // against a 20W ceiling (typical BH GDDR active power budget).
                let mvddq_norm: f32 = smbus
                    .and_then(|s| s.mvddq_power.as_deref())
                    .and_then(|v| parse_hex_or_dec(v))
                    .map(|v| (v as f32 / 20.0).clamp(0.0, 1.0))
                    .unwrap_or(0.0);

                if d_idx > 0 { spans.push(Span::raw(" ".repeat(gutter))); }

                if row == 0 {
                    self.render_device_label(&mut spans, device, ds, power, temp, aiclk, grid_w);
                } else if row == 1 {
                    self.render_power_bar(&mut spans, ds, power, temp, grid_w);
                } else {
                    let ch_row = row - header_rows;
                    let ch_idx = ch_row / rows_per_channel;
                    let sub_row = ch_row % rows_per_channel;
                    if ch_idx >= GDDR_CHANNELS {
                        spans.push(Span::raw(" ".repeat(grid_w)));
                        continue;
                    }
                    self.render_channel_row(&mut spans, ds, idx, ch_idx, sub_row, rows_per_channel, grid_w, temp, mvddq_norm);
                }
            }
            lines.push(Line::from(spans));
        }

        lines
    }

    fn render_device_label(
        &self,
        spans: &mut Vec<Span<'static>>,
        device: &crate::models::Device,
        ds: Option<&DeviceState>,
        power: f32,
        temp: f32,
        aiclk: u32,
        grid_w: usize,
    ) {
        let phase = ds.map(|s| s.phase).unwrap_or(Phase::Idle);
        let mean_fill = ds.map(|s| s.mean_fill()).unwrap_or(0.0);
        let eth = ds.map(|s| s.eth_links).unwrap_or(0);
        let hb_pulse = ds.map(|s| s.heartbeat_pulse).unwrap_or(0);

        // All phase labels are exactly PHASE_W display columns wide.
        // Use this constant (not .len()) for layout math — the strings contain
        // multibyte chars so byte length != display width.
        const PHASE_W: usize = 9; // 1 leading space + 8 label chars
        let phase_str = match phase {
            Phase::Init           => " INIT    ",  // 9 display cols
            Phase::Dma            => " DMA ←←  ",  // 9 display cols
            Phase::Running        => " RUN ▶▶  ",  // 9 display cols
            Phase::Idle           => " idle    ",  // 9 display cols
            Phase::Deconstructing => " EVICT ░ ",  // 9 display cols
        };
        let phase_color = match phase {
            Phase::Init           => colors::rgb(120, 120, 180),
            Phase::Dma            => colors::rgb(255, 200, 60),
            Phase::Running        => colors::rgb(80, 220, 140),
            Phase::Idle           => colors::rgb(80, 80, 100),
            Phase::Deconstructing => colors::rgb(220, 80, 80),
        };

        // Heartbeat indicator: pulses on ARC tick
        let hb_char = if hb_pulse > 0 { "♥ " } else { "· " };
        let hb_color = if hb_pulse > 0 { colors::rgb(220, 80, 100) } else { colors::rgb(50, 50, 60) };

        // Fixed-width stats so the label never shifts.
        // hb(2) + label + phase(PHASE_W) must fit in grid_w.
        let label = format!(
            "{}{} {:>3}% {:>4}W {:>4}MHz e{}",
            device.architecture.abbrev(), device.index,
            (mean_fill * 100.0) as u32, power as u32, aiclk, eth,
        );
        let label_budget = grid_w.saturating_sub(2 + PHASE_W);
        let truncated = if label.len() > label_budget {
            // Truncate on a char boundary
            let mut end = label_budget;
            while end > 0 && !label.is_char_boundary(end) { end -= 1; }
            label[..end].to_string()
        } else {
            label
        };

        let hue = temp_to_hue(temp);
        let label_color = hsv_to_rgb(hue, 0.5, 0.85);

        spans.push(Span::styled(hb_char.to_string(), Style::default().fg(hb_color)));
        spans.push(Span::styled(truncated, Style::default().fg(label_color).add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(
            phase_str.to_string(),
            Style::default().fg(phase_color).add_modifier(Modifier::BOLD),
        ));
    }

    fn render_power_bar(
        &self,
        spans: &mut Vec<Span<'static>>,
        ds: Option<&DeviceState>,
        power: f32,
        temp: f32,
        grid_w: usize,
    ) {
        let pcie = ds.map(|s| s.pcie_usage).unwrap_or(0);
        let pcie_str = if pcie >= 16 { "x16" } else if pcie >= 8 { " x8" } else if pcie >= 4 { " x4" } else { "   " };
        // Power bar: 0–200W range (TDP on BH can hit 160W+)
        let bar_w = grid_w.saturating_sub(11);
        let pct = (power / 200.0).clamp(0.0, 1.0);
        let filled = (pct * bar_w as f32) as usize;
        let hue = temp_to_hue(temp);
        let color = hsv_to_rgb(hue, 0.8, 0.8);
        let dim   = hsv_to_rgb(hue, 0.4, 0.35);

        let prefix = format!("{:4.0}W {} ", power, pcie_str);
        spans.push(Span::raw(prefix));
        // Build filled and empty segments as separate strings — avoids byte-slicing
        // multibyte chars (█ and ░ are 3 bytes each).
        if filled > 0 {
            let s: String = std::iter::repeat('█').take(filled).collect();
            spans.push(Span::styled(s, Style::default().fg(color)));
        }
        if filled < bar_w {
            let s: String = std::iter::repeat('░').take(bar_w - filled).collect();
            spans.push(Span::styled(s, Style::default().fg(dim)));
        }
    }

    fn render_channel_row(
        &self,
        spans: &mut Vec<Span<'static>>,
        ds: Option<&DeviceState>,
        device_idx: usize,
        ch_idx: usize,
        sub_row: usize,
        _rows_per_channel: usize,
        grid_w: usize,
        asic_temp: f32,
        mvddq_norm: f32,
    ) {
        let ch = ds.and_then(|s| s.channels.get(ch_idx));
        let phase = ds.map(|s| s.phase).unwrap_or(Phase::Idle);

        let enabled = ch.map(|c| c.enabled).unwrap_or(true);
        let trained = ch.map(|c| c.trained).unwrap_or(false);
        let fill    = ch.map(|c| c.fill).unwrap_or(0.0);
        let head    = ch.map(|c| c.head_pos).unwrap_or(0.0);
        let ch_temp = ch.map(|c| c.temp_ema).unwrap_or(asic_temp);
        let err_flash = ch.map(|c| c.err_flash).unwrap_or(0);

        if !enabled {
            // Disabled channel: hatched
            let s: String = (0..grid_w).map(|_| '╌').collect();
            spans.push(Span::styled(s, Style::default().fg(colors::rgb(30, 30, 40))));
            return;
        }

        // Channel hue: evenly spaced rainbow ch0=lime-green → ch7=hot-pink,
        // matching the screenshot palette.  Temperature is kept for label color
        // only — block fill uses a clean channel-index hue so rows are instantly
        // distinguishable regardless of thermal state.
        let ch_hue = 100.0 + ch_idx as f32 * (210.0 / (GDDR_CHANNELS - 1) as f32); // 100°→310°
        let base_hue = ch_hue; // used for blocks; label uses temp-tinted variant below
        let label_hue = (temp_to_hue(ch_temp) + ch_idx as f32 * 26.0) % 360.0;

        // Channel label (first sub-row only, 2 chars wide)
        let label_w = if sub_row == 0 && grid_w > 4 { 2 } else { 0 };
        if label_w > 0 {
            let lc = hsv_to_rgb(label_hue, 0.4, 0.55);
            spans.push(Span::styled(
                format!("{} ", ch_idx),
                Style::default().fg(lc),
            ));
        }

        let cells = grid_w.saturating_sub(label_w);
        let resident_cells = (fill * cells as f32) as usize;
        // Frontier: 2 cells wide during DMA only
        let frontier_w: usize = if phase == Phase::Dma { 2 } else { 0 };
        // Seek zone: sparse dots ahead of frontier
        let seek_zone_end = (resident_cells + frontier_w + cells / 8).min(cells);

        // Per-channel cursor speed — prime multipliers make each channel move differently.
        let primes_spd: [u64; 8] = [2, 3, 5, 7, 11, 13, 17, 19];
        let power_factor = ds.map(|s| (s.power_ema / 80.0).clamp(0.4, 2.5)).unwrap_or(1.0);
        let base_fpc = (4.0 / power_factor).round().max(1.0) as u64;
        let cursor_frames_per_cell = (base_fpc * primes_spd[ch_idx] / primes_spd[0]).max(1);
        let scan_period = cells.max(1) as u64;
        let ch_offset = ch_idx as u64 * 37 + 11;
        let scan_pos = (self.frame / cursor_frames_per_cell + ch_offset) % scan_period.max(1);

        // Slow global pulse — different period per channel.
        let wave1_period = 90u64 + ch_idx as u64 * 23;

        // Deterministic segment map — stable per device+channel+position.
        // Brightness ceiling deliberately kept dark (max 0.62) so nothing approaches white.
        // Block character (░▒▓█) carries the "how full / how active" signal, not value.
        let device_seed = device_idx;
        let seg_brightness = |col: usize| -> f32 {
            let seg_w = 6 + ((ch_idx * 3 + col / 6) % 4) * 2;
            let seg_idx = col / seg_w.max(1);
            let h = (device_seed.wrapping_mul(7919) ^ ch_idx.wrapping_mul(1009) ^ seg_idx.wrapping_mul(6271)) % 16;
            match h % 4 {
                0 => 0.28, // very dim — cold / unused segment
                1 => 0.42, // dim
                2 => 0.62, // mid — "hot" segment ceiling (was 0.82)
                _ => 0.35, // default
            }
        };
        let seg_saturation = |col: usize| -> f32 {
            let seg_w = 6 + ((ch_idx * 3 + col / 6) % 4) * 2;
            let seg_idx = col / seg_w.max(1);
            let h = (device_seed.wrapping_mul(3571) ^ ch_idx.wrapping_mul(2017) ^ seg_idx.wrapping_mul(4093)) % 8;
            if h == 0 { 0.10 } else { 0.80 + (h % 3) as f32 * 0.05 }
        };

        // "Build from darkness" factor: newly written cells start near-black and
        // warm up over ~400 frames.  Based on fill progress — cells near the write
        // frontier are freshest (darkest); deep cells have had time to "settle".
        // In Running phase, all cells are fully settled → factor = 1.0.
        let age_factor = |col: usize| -> f32 {
            if phase != Phase::Dma || resident_cells == 0 { return 1.0; }
            // Fraction of cells written so far
            let written_frac = fill;
            // This cell's position as fraction of total
            let col_frac = col as f32 / cells.max(1) as f32;
            // Distance behind the frontier: 0 = at frontier, 1 = at start
            let lag = (written_frac - col_frac).clamp(0.0, 1.0);
            // Cells far behind the frontier are older and brighter
            // Cells close to the frontier are freshest (darkest) → ramp up
            let ramp_width = 0.25_f32; // front 25% of fill is still warming
            if lag < ramp_width {
                (lag / ramp_width).powi(2) * 0.85 + 0.05 // 0.05 → 0.90 quadratic
            } else {
                1.0
            }
        };

        // Span coalescer: accumulates chars with the same style into one Span.
        // Avoids O(cells) heap allocations — adjacent same-style cells share one String.
        let mut run_str = String::with_capacity(cells * 3); // 3 bytes per block char
        let mut run_style: Option<Style> = None;
        let mut flush = |spans: &mut Vec<Span<'static>>, s: &mut String, st: &mut Option<Style>| {
            if !s.is_empty() {
                spans.push(Span::styled(
                    std::mem::take(s),
                    st.unwrap_or_default(),
                ));
                *st = None;
            }
        };

        // --- Error flash cell (if active) ---
        let flash_col = if err_flash > 0 { Some(resident_cells.saturating_sub(1)) } else { None };

        // --- Resident region (col 0..resident_cells) ---
        for col in 0..resident_cells.min(cells) {
            let (glyph, style) = if flash_col == Some(col) {
                // Error flash: dark red ✗ — not white, uses block glyph intensity
                let intensity = err_flash as f32 / 12.0 * 0.55; // cap at 0.55
                let ec = hsv_to_rgb(0.0, 1.0, intensity);
                ('✗', Style::default().fg(ec).add_modifier(Modifier::BOLD))
            } else {
                match phase {
                    Phase::Running => {
                        let dist_to_scan = (col as i64 - scan_pos as i64).unsigned_abs() as u64;
                        if dist_to_scan == 0 {
                            // Cursor head: mid-brightness ▒ — marks position without glowing
                            let c = hsv_to_rgb(base_hue, 0.90, 0.65);
                            ('▒', Style::default().fg(c))
                        } else if dist_to_scan == 1 {
                            // Shoulder: slightly darker
                            let c = hsv_to_rgb(base_hue, 0.85, 0.45);
                            ('░', Style::default().fg(c))
                        } else {
                            // Body: block character encodes activity level.
                            // mvddq lifts the char tier, not the brightness value.
                            let t1 = self.frame % wave1_period;
                            let w1 = (t1 as f32 / wave1_period as f32 * std::f32::consts::TAU).sin();
                            let base_v = seg_brightness(col);
                            let mem_lift = mvddq_norm * 0.12;
                            let v = (base_v + 0.05 * w1 + mem_lift).clamp(0.18, 0.70);
                            let s = seg_saturation(col);
                            let c = hsv_to_rgb(base_hue, s, v);
                            // Block char tier driven by combined brightness — darker = ░, brighter = █
                            let glyph = if v > 0.55 { '▓' } else if v > 0.38 { '▒' } else { '░' };
                            (glyph, Style::default().fg(c))
                        }
                    }
                    Phase::Deconstructing => {
                        let dist_from_edge = resident_cells.saturating_sub(1 + col);
                        let flicker = (self.frame as u8)
                            .wrapping_add(col as u8 * 17)
                            .wrapping_add(ch_idx as u8 * 31);
                        if dist_from_edge == 0 {
                            // Eviction edge: dark red, no white flash
                            let v = if flicker % 3 == 0 { 0.60 } else { 0.38 };
                            ('▓', Style::default().fg(hsv_to_rgb(0.0, 1.0, v)))
                        } else if dist_from_edge < 4 {
                            let v = (0.38 - dist_from_edge as f32 * 0.06).max(0.10);
                            ('▒', Style::default().fg(hsv_to_rgb(15.0, 0.9, v)))
                        } else {
                            // Deep: quantized dark blocks in channel hue, fading to black
                            let v_raw = (0.28 - dist_from_edge as f32 * 0.015).clamp(0.05, 0.32);
                            let v = (v_raw * 8.0).round() / 8.0;
                            let glyph = if v > 0.20 { '░' } else { ' ' };
                            (glyph, Style::default().fg(hsv_to_rgb(base_hue, 0.5, v)))
                        }
                    }
                    _ => {
                        // DMA fill / Idle / Init: age_factor builds from darkness.
                        // Char tier encodes maturity of the written region.
                        let v = (seg_brightness(col) * age_factor(col)).clamp(0.05, 0.65);
                        let s = seg_saturation(col);
                        let glyph = if v > 0.50 { '▓' } else if v > 0.30 { '▒' } else if v > 0.12 { '░' } else { ' ' };
                        (glyph, Style::default().fg(hsv_to_rgb(base_hue, s, v)))
                    }
                }
            };
            if run_style == Some(style) {
                run_str.push(glyph);
            } else {
                flush(spans, &mut run_str, &mut run_style);
                run_style = Some(style);
                run_str.push(glyph);
            }
        }

        // --- DMA frontier (2 cells, only during Dma) ---
        // The write head is darker than the settled region behind it — light builds
        // up as cells age, not at the leading edge.
        if phase == Phase::Dma {
            for col in resident_cells..(resident_cells + frontier_w).min(cells) {
                let dist = col - resident_cells;
                let phase_val = (self.frame as u8)
                    .wrapping_add(col as u8 * 13)
                    .wrapping_add(ch_idx as u8 * 41);
                // dist 0 = write head (darkest), dist 1 = just behind (slightly more settled)
                let v = if dist == 0 {
                    if phase_val % 4 < 2 { 0.28 } else { 0.22 }
                } else {
                    0.35
                };
                let fc = if phase_val % 3 == 0 { '▒' } else { '░' };
                let style = Style::default().fg(hsv_to_rgb(base_hue, 0.75, v));
                if run_style == Some(style) {
                    run_str.push(fc);
                } else {
                    flush(spans, &mut run_str, &mut run_style);
                    run_style = Some(style);
                    run_str.push(fc);
                }
            }

            // --- DMA seek zone: dots then empty — emit as one or two spans ---
            let seek_start = (resident_cells + frontier_w).min(cells);
            let dot_style  = Style::default().fg(colors::rgb(40, 55, 75));
            let dark_style = Style::default().fg(colors::rgb(25, 30, 40));
            for col in seek_start..seek_zone_end.min(cells) {
                let noise = (col as u64 * 7 + ch_idx as u64 * 13 + self.frame / 3) % 11;
                let (gc, style) = if noise == 0 {
                    ('·', dot_style)
                } else if !trained {
                    ('░', dark_style)
                } else {
                    (' ', Style::default())
                };
                if run_style == Some(style) {
                    run_str.push(gc);
                } else {
                    flush(spans, &mut run_str, &mut run_style);
                    run_style = Some(style);
                    run_str.push(gc);
                }
            }
        }

        // --- Init pattern (untrained cells before resident zone) ---
        if phase == Phase::Init && !trained {
            let init_start = resident_cells.min(cells);
            for col in init_start..cells {
                let t = (self.frame / 4 + col as u64 + ch_idx as u64 * 3) % 8;
                let (gc, v) = if t == 0 { ('░', 0.18) } else { (' ', 0.0) };
                let style = Style::default().fg(hsv_to_rgb(200.0, 0.25, v));
                if run_style == Some(style) {
                    run_str.push(gc);
                } else {
                    flush(spans, &mut run_str, &mut run_style);
                    run_style = Some(style);
                    run_str.push(gc);
                }
            }
            flush(spans, &mut run_str, &mut run_style);
            return;
        }

        // --- Empty tail: all remaining cells as a SINGLE span ---
        flush(spans, &mut run_str, &mut run_style);
        let empty_start = match phase {
            Phase::Dma  => seek_zone_end.min(cells),
            _           => resident_cells.min(cells),
        };
        if empty_start < cells {
            let count = cells - empty_start;
            let s: String = std::iter::repeat('░').take(count).collect();
            spans.push(Span::styled(s, Style::default().fg(colors::rgb(18, 24, 32))));
        }
    }

    fn render_header(&self, backend: &dyn TelemetryBackend) -> Line<'static> {
        let n = backend.devices().len();
        let (ndma, nrun, nidle) = backend.devices().iter().fold((0u32,0u32,0u32), |acc, dev| {
            let ph = self.devices.get(&dev.index).map(|s| s.phase).unwrap_or(Phase::Idle);
            match ph {
                Phase::Dma     => (acc.0+1, acc.1, acc.2),
                Phase::Running => (acc.0, acc.1+1, acc.2),
                _              => (acc.0, acc.1, acc.2+1),
            }
        });

        // Always render all three counters with fixed width so the header never shifts.
        // Format: "N×DMA " — N is always 1 digit (max 8 chips), label is fixed 3 chars.
        let dma_color  = if ndma  > 0 { colors::rgb(255, 200, 60)  } else { colors::rgb(50, 55, 45)  };
        let run_color  = if nrun  > 0 { colors::rgb(80, 220, 140)  } else { colors::rgb(40, 60, 50)  };
        let idle_color = if nidle > 0 { colors::rgb(120, 120, 140) } else { colors::rgb(40, 40, 50)  };
        let dma_mod  = if ndma  > 0 { Modifier::BOLD } else { Modifier::DIM };
        let run_mod  = if nrun  > 0 { Modifier::BOLD } else { Modifier::DIM };
        let idle_mod = if nidle > 0 { Modifier::empty() } else { Modifier::DIM };

        Line::from(vec![
            Span::styled("  DEFRAG ", Style::default().fg(colors::rgb(100, 220, 255)).add_modifier(Modifier::BOLD)),
            Span::styled("│ ", Style::default().fg(colors::rgb(50, 70, 90))),
            Span::styled(format!("{} device{}  ", n, if n == 1 { "" } else { "s" }), Style::default().fg(colors::rgb(150, 150, 170))),
            Span::styled("│ ", Style::default().fg(colors::rgb(50, 70, 90))),
            Span::styled(format!("{}×DMA  ", ndma), Style::default().fg(dma_color).add_modifier(dma_mod)),
            Span::styled(format!("{}×RUN  ", nrun), Style::default().fg(run_color).add_modifier(run_mod)),
            Span::styled(format!("{}×idle  ", nidle), Style::default().fg(idle_color).add_modifier(idle_mod)),
            Span::styled("│  ", Style::default().fg(colors::rgb(50, 70, 90))),
            Span::styled("█ ", Style::default().fg(colors::rgb(80, 200, 130))),
            Span::styled("resident  ", Style::default().fg(colors::rgb(100, 120, 100))),
            Span::styled("▓ ", Style::default().fg(colors::rgb(255, 200, 60))),
            Span::styled("writing  ", Style::default().fg(colors::rgb(120, 110, 80))),
            Span::styled("░ ", Style::default().fg(colors::rgb(35, 45, 55))),
            Span::styled("empty", Style::default().fg(colors::rgb(80, 90, 100))),
        ])
    }

    fn render_footer(&self) -> Line<'static> {
        Line::from(vec![
            Span::styled("  d ", Style::default().fg(colors::rgb(100, 220, 255)).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
            Span::styled("defrag  ", Style::default().fg(colors::rgb(120, 120, 140))),
            Span::styled("│ ", Style::default().fg(colors::rgb(50, 70, 90))),
            Span::styled(" v ", Style::default().fg(colors::rgb(80, 220, 200)).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
            Span::styled("cycle  ", Style::default().fg(colors::rgb(120, 120, 140))),
            Span::styled("│ ", Style::default().fg(colors::rgb(50, 70, 90))),
            Span::styled(" / ", Style::default().fg(colors::rgb(220, 180, 80)).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
            Span::styled("command", Style::default().fg(colors::rgb(120, 120, 140))),
        ])
    }
}

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
