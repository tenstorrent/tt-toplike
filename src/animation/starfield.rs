// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Hardware-responsive starfield visualization
//!
//! This module implements the core visualization where Tensix cores become stars,
//! memory channels become planets, and all motion is driven by real hardware telemetry.
//!
//! Key principles:
//! - Star positions match actual chip topology (GS: 10×12, WH: 8×10, BH: 14×16)
//! - Color = Temperature (cyan→green→yellow→orange→red)
//! - Brightness = Power consumption
//! - Twinkle rate = Current draw
//! - No fake animations - everything driven by real hardware state

use crate::animation::topology::{sync_score, BoardTopology};
use crate::animation::AdaptiveBaseline;
use crate::backend::TelemetryBackend;
use crate::models::Device;
use crate::ui::colors;
use ratatui::style::Color;
use std::collections::HashMap;
use std::f32;

/// Character progression for star brightness levels
const STAR_CHARS: [char; 5] = ['·', '∘', '○', '◉', '●'];

/// Character progression for memory planet intensity
const PLANET_CHARS: [char; 5] = ['·', '░', '▒', '▓', '█'];

// FLOW_CHARS removed: DataStream now uses topology-aware '─' (intra-board) / '═' (inter-board).

/// A single star representing a Tensix core
#[derive(Debug, Clone)]
pub struct Star {
    /// X position in terminal coordinates
    pub x: usize,

    /// Y position in terminal coordinates
    pub y: usize,

    /// Device index this star belongs to
    pub device_idx: usize,

    /// Core index within the device
    pub core_idx: usize,

    /// Current brightness (0.0 = dim, 1.0 = bright)
    pub brightness: f32,

    /// Current color (temperature-based)
    pub color: Color,

    /// Animation phase (for twinkling)
    pub phase: f32,

    /// Z-depth layer (0.0 = far background, 1.0 = foreground)
    /// Used for parallax effect and size variation
    pub depth: f32,

    /// Secondary twinkle phase (different frequency for complexity)
    pub phase2: f32,

    /// Sparkle counter (for occasional bright flashes)
    pub sparkle: f32,
}

impl Star {
    /// Get the character to render based on brightness and depth
    ///
    /// Foreground stars (depth close to 1.0) render larger/brighter
    /// Background stars (depth close to 0.0) render smaller/dimmer
    pub fn get_char(&self) -> char {
        // Sparkle effect - occasionally show as brightest character
        if self.sparkle > 0.8 {
            return '✦'; // Bright sparkle
        }

        // Depth affects perceived brightness (foreground = brighter)
        let depth_brightness = self.brightness * (0.4 + self.depth * 0.6);

        let idx = (depth_brightness * (STAR_CHARS.len() - 1) as f32) as usize;
        STAR_CHARS[idx.min(STAR_CHARS.len() - 1)]
    }
}

/// A memory planet representing L1/L2/DDR hierarchy
#[derive(Debug, Clone)]
pub struct MemoryPlanet {
    /// X position in terminal coordinates
    pub x: usize,

    /// Y position in terminal coordinates
    pub y: usize,

    /// Device index this planet belongs to
    pub device_idx: usize,

    /// Memory level (0=L1, 1=L2, 2=DDR)
    pub level: usize,

    /// Channel index for DDR, cache index for L1/L2
    pub channel_idx: usize,

    /// Current activity level (0.0 to 1.0)
    pub activity: f32,

    /// Current color
    pub color: Color,

    /// Orbital angle (0.0 to 2π) for circular motion around device
    pub angle: f32,

    /// Orbital radius from device center
    pub radius: f32,

    /// Pulsing phase for size animation
    pub pulse: f32,
}

impl MemoryPlanet {
    /// Get the character to render based on activity
    pub fn get_char(&self) -> char {
        match self.level {
            0 => '◆', // L1 - diamond
            1 => '◇', // L2 - outline diamond
            2 => {
                // DDR - intensity-based blocks
                let idx = (self.activity * (PLANET_CHARS.len() - 1) as f32) as usize;
                PLANET_CHARS[idx.min(PLANET_CHARS.len() - 1)]
            }
            _ => '·',
        }
    }

    /// Get color for this memory level
    pub fn get_color(&self) -> Color {
        match self.level {
            0 => colors::rgb(100, 180, 255), // L1 - blue (was colors::info())
            1 => colors::rgb(255, 180, 100), // L2 - orange (was colors::warning())
            2 => colors::rgb(255, 100, 100), // DDR - red (was colors::error())
            _ => colors::rgb(160, 160, 160), // Fallback grey
        }
    }
}

/// A data flow stream between devices
#[derive(Debug, Clone)]
pub struct DataStream {
    /// X position in terminal coordinates
    pub x: usize,

    /// Y position in terminal coordinates
    pub y: usize,

    /// Source device index
    pub from_device: usize,

    /// Target device index
    pub to_device: usize,

    /// Flow intensity / sync score (0.0 to 1.0)
    pub intensity: f32,

    /// Animation offset
    pub offset: f32,

    /// Whether both endpoints are on the same physical board.
    /// Intra-board streams use `─` and are always-on (floor 0.2).
    /// Inter-board streams use `═` and only appear when both sides are active.
    pub intra_board: bool,
}

impl DataStream {
    /// Get the character to render based on topology.
    ///
    /// `─` for intra-board links (always visible, even when idle).
    /// `═` for inter-board links (heavier, only shown when active).
    pub fn get_char(&self) -> char {
        if self.intra_board {
            '─'
        } else {
            '═'
        }
    }
}

/// Hardware-responsive starfield visualization system
pub struct HardwareStarfield {
    /// All stars (Tensix cores)
    stars: Vec<Star>,

    /// All memory planets (L1/L2/DDR)
    planets: Vec<MemoryPlanet>,

    /// Data flow streams between devices
    streams: Vec<DataStream>,

    /// Adaptive baseline for relative activity detection
    baseline: AdaptiveBaseline,

    /// Animation frame counter
    frame: u32,

    /// Display width in characters
    width: usize,

    /// Display height in characters
    height: usize,

    /// Board topology (set once after backend init).
    /// Drives stream character selection and intra-board floor.
    board_topology: Option<BoardTopology>,

    /// Snapshot of per-device baseline-relative power change,
    /// updated each frame by `update_from_telemetry`.
    /// Used by stream rendering to compute `sync_score`.
    device_activity: HashMap<usize, f32>,

    /// Per-device temperature (°C) — populated by update_from_telemetry.
    /// Used by the fleet heat-map renderer when device count is too large for
    /// individual star grids.
    device_temps: HashMap<usize, f32>,

    /// Per-device power (W) — populated by update_from_telemetry.
    device_powers: HashMap<usize, f32>,

    /// Number of devices the starfield was last initialised with.
    /// Controls fleet-mode threshold check in render().
    device_count: usize,

    /// Render section chrome (the standalone header label).  `true` for the
    /// standalone Starfield mode (default, byte-identical to today); Arcade sets
    /// this `false` so the composite view owns a single shared telemetry strip
    /// instead of each embedded section printing its own header.
    chrome: bool,
}

impl HardwareStarfield {
    /// Create a new hardware starfield
    ///
    /// # Arguments
    ///
    /// * `width` - Display width in terminal characters
    /// * `height` - Display height in terminal characters
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            stars: Vec::new(),
            planets: Vec::new(),
            streams: Vec::new(),
            baseline: AdaptiveBaseline::new(),
            frame: 0,
            width,
            height,
            board_topology: None,
            device_activity: HashMap::new(),
            device_temps: HashMap::new(),
            device_powers: HashMap::new(),
            device_count: 0,
            chrome: true,
        }
    }

    /// Toggle section chrome (header label).  `true` = standalone look
    /// (default); `false` suppresses the section header so a composite view
    /// (Arcade) can render one shared telemetry strip instead.
    pub fn set_chrome(&mut self, chrome: bool) {
        self.chrome = chrome;
    }

    /// Set the animation sensitivity multiplier.
    ///
    /// Delegates directly to the internal `AdaptiveBaseline`. Higher values make
    /// star brightness and twinkle respond to smaller hardware fluctuations.
    /// 1.0 = Normal, 5.0 = Paranoid, 0.2 = AnomaliesOnly.
    pub fn set_sensitivity(&mut self, sensitivity: f32) {
        self.baseline.sensitivity = sensitivity;
    }

    /// Install board topology for topology-aware stream rendering.
    ///
    /// Call this once after backend initialisation when SMBUS `board_id`
    /// data may be available.  Safe to call at any point — the change
    /// takes effect on the next `update_from_telemetry` call.
    pub fn set_topology(&mut self, topology: BoardTopology) {
        self.board_topology = Some(topology);
        // Re-tag all existing streams so they don't need a full reinit.
        if let Some(ref topo) = self.board_topology {
            for stream in &mut self.streams {
                stream.intra_board = topo.same_board(stream.from_device, stream.to_device);
            }
        }
    }

    /// Access the current board topology, if set.
    pub fn topology(&self) -> Option<&BoardTopology> {
        self.board_topology.as_ref()
    }

    /// Initialize stars and planets from device topology
    ///
    /// Creates star positions based on actual Tensix grid layout for each architecture.
    ///
    /// # Arguments
    ///
    /// * `devices` - Slice of detected devices
    pub fn initialize_from_devices(&mut self, devices: &[Device]) {
        self.stars.clear();
        self.planets.clear();

        let num_devices = devices.len();
        self.device_count = num_devices;
        if num_devices == 0 {
            return;
        }

        // Fleet galaxy mode: 32+ devices can't be individually resolved at usable
        // terminal widths.  Fleet render() draws a galaxy-style heat-map instead —
        // skip star init so individual stars aren't rendered degenerate.
        if num_devices >= 32 {
            return;
        }

        // Calculate device spacing across screen width
        let device_spacing = self.width / num_devices.max(1);

        for (device_idx, device) in devices.iter().enumerate() {
            // Calculate center position for this device (horizontal only)
            let device_center_x = (device_idx * device_spacing) + (device_spacing / 2);

            // Get Tensix grid dimensions for this architecture
            let (grid_rows, grid_cols) = device.tensix_grid();

            // Cell spacing: at least 1 so no rows/cols collapse in small panels
            // (arcade starfield can be as few as 8 rows).
            let star_spacing_x = ((device_spacing.saturating_sub(2)) / grid_cols.max(1)).max(1);
            let star_spacing_y = (self.height / grid_rows.max(1)).max(1);

            // Span = distance from the first dot to the last dot (dot-center to dot-center).
            // Centering the span gives equal whitespace above/below and left/right of the grid.
            let grid_span_x = (grid_cols.saturating_sub(1)) * star_spacing_x;
            let grid_span_y = (grid_rows.saturating_sub(1)) * star_spacing_y;

            let x_start = device_center_x.saturating_sub(grid_span_x / 2);
            let y_start = (self.height.saturating_sub(grid_span_y)) / 2;

            // Planets orbit the geometric centre of the star grid.
            let device_center_y = y_start + grid_span_y / 2;

            // Create stars for each Tensix core
            for row in 0..grid_rows {
                for col in 0..grid_cols {
                    let x = x_start + col * star_spacing_x;
                    let y = y_start + row * star_spacing_y;

                    // Ensure within bounds
                    if x < self.width && y < self.height {
                        // Assign varied z-depth for parallax effect
                        // Core position influences depth (edge cores = background, center = foreground)
                        let dist_from_center_x = (col as f32 - grid_cols as f32 / 2.0).abs();
                        let dist_from_center_y = (row as f32 - grid_rows as f32 / 2.0).abs();
                        let depth = 1.0
                            - ((dist_from_center_x + dist_from_center_y)
                                / (grid_cols as f32 + grid_rows as f32))
                                * 0.6;

                        self.stars.push(Star {
                            x,
                            y,
                            device_idx,
                            core_idx: row * grid_cols + col,
                            brightness: 0.3, // Start dim
                            color: colors::primary(),
                            phase: (row + col) as f32 * 0.5, // Varied animation phases
                            depth: depth.clamp(0.3, 1.0),
                            phase2: (row * 3 + col * 7) as f32 * 0.3, // Different frequency
                            sparkle: 0.0,
                        });
                    }
                }
            }

            // Create memory planets around device perimeter
            let num_channels = device.memory_channels();

            // DDR planets (outer ring)
            for i in 0..num_channels {
                let angle = (i as f32 / num_channels as f32) * 2.0 * f32::consts::PI;
                let radius = (device_spacing / 2).min(self.height / 3) as f32;
                let px = device_center_x as f32 + angle.cos() * radius;
                let py = device_center_y as f32 + angle.sin() * radius;

                if px >= 0.0
                    && (px as usize) < self.width
                    && py >= 0.0
                    && (py as usize) < self.height
                {
                    self.planets.push(MemoryPlanet {
                        x: px as usize,
                        y: py as usize,
                        device_idx,
                        level: 2, // DDR
                        channel_idx: i,
                        activity: 0.0,
                        color: colors::error(),
                        angle,
                        radius,
                        pulse: i as f32 * 0.4, // Varied pulsing phases
                    });
                }
            }

            // L2 planets (middle ring) - 8 cache banks
            for i in 0..8 {
                let angle = (i as f32 / 8.0) * 2.0 * f32::consts::PI + 0.4; // Offset from DDR
                let radius = (device_spacing / 3).min(self.height / 4) as f32;
                let px = device_center_x as f32 + angle.cos() * radius;
                let py = device_center_y as f32 + angle.sin() * radius;

                if px >= 0.0
                    && (px as usize) < self.width
                    && py >= 0.0
                    && (py as usize) < self.height
                {
                    self.planets.push(MemoryPlanet {
                        x: px as usize,
                        y: py as usize,
                        device_idx,
                        level: 1, // L2
                        channel_idx: i,
                        activity: 0.0,
                        color: colors::warning(),
                        angle,
                        radius,
                        pulse: i as f32 * 0.5 + 0.3, // Varied pulsing phases
                    });
                }
            }

            // L1 planets (inner ring) - 4 SRAM banks
            for i in 0..4 {
                let angle = (i as f32 / 4.0) * 2.0 * f32::consts::PI + 0.8; // Offset from L2
                let radius = (device_spacing / 5).min(self.height / 6) as f32;
                let px = device_center_x as f32 + angle.cos() * radius;
                let py = device_center_y as f32 + angle.sin() * radius;

                if px >= 0.0
                    && (px as usize) < self.width
                    && py >= 0.0
                    && (py as usize) < self.height
                {
                    self.planets.push(MemoryPlanet {
                        x: px as usize,
                        y: py as usize,
                        device_idx,
                        level: 0, // L1
                        channel_idx: i,
                        activity: 0.0,
                        color: colors::info(),
                        angle,
                        radius,
                        pulse: i as f32 * 0.7 + 0.6, // Varied pulsing phases
                    });
                }
            }
        }

        // Initialize data streams between devices (if multiple devices)
        if num_devices > 1 {
            let device_spacing = self.width / num_devices;
            for from_idx in 0..num_devices {
                for to_idx in (from_idx + 1)..num_devices {
                    let from_x = (from_idx * device_spacing) + (device_spacing / 2);
                    let to_x = (to_idx * device_spacing) + (device_spacing / 2);

                    // Determine topology relationship for this stream pair.
                    let intra = self
                        .board_topology
                        .as_ref()
                        .map(|t| t.same_board(from_idx, to_idx))
                        .unwrap_or(false);

                    // Create stream segments along the horizontal path.
                    let num_steps = ((to_x - from_x) / 5).max(1);
                    for step in 0..num_steps {
                        let x = from_x + (to_x - from_x) * step / num_steps;
                        let y = self.height / 2;

                        if x < self.width && y < self.height {
                            self.streams.push(DataStream {
                                x,
                                y,
                                from_device: from_idx,
                                to_device: to_idx,
                                intensity: 0.0,
                                offset: step as f32 / num_steps as f32,
                                intra_board: intra,
                            });
                        }
                    }
                }
            }
        }
    }

    /// Update visualization from current hardware telemetry
    ///
    /// This is where the magic happens - every visual element gets updated
    /// based on real hardware state.
    ///
    /// # Arguments
    ///
    /// * `backend` - Backend providing telemetry data
    pub fn update_from_telemetry(&mut self, backend: &dyn TelemetryBackend) {
        // Update baseline learning and snapshot per-device activity.
        for device in backend.devices() {
            if let Some(telem) = backend.telemetry(device.index) {
                let power = telem.power_w();
                let current = telem.current_a();
                let temp = telem.temp_c();
                let aiclk = telem.aiclk_mhz() as f32;

                self.baseline
                    .update(device.index, power, current, temp, aiclk);

                // Cache baseline-relative power change for stream sync_score.
                let activity = self
                    .baseline
                    .power_change(device.index, power)
                    .max(0.0)
                    .min(1.0);
                self.device_activity.insert(device.index, activity);

                // Cache raw temp/power for fleet heat-map rendering.
                self.device_temps.insert(device.index, temp);
                self.device_powers.insert(device.index, power);
            }
        }

        // Update star properties based on telemetry
        for star in &mut self.stars {
            if let Some(telem) = backend.telemetry(star.device_idx) {
                let smbus = backend.smbus_telemetry(star.device_idx);
                let power = telem.power_w();
                let current = telem.current_a();
                let temp = telem.temp_c();

                // Throttler: non-zero means the chip is actively being held back.
                // Parse as bitmask — any set bit = some throttle condition active.
                let throttler: u32 = smbus
                    .and_then(|s| s.throttler.as_deref())
                    .and_then(|v| crate::models::telemetry::parse_hex_or_dec(v))
                    .unwrap_or(0);
                // throttle_factor: 0.0 = fully throttled, 1.0 = free running.
                // Each set throttle bit costs ~20% brightness — a heavy throttle
                // visibly darkens the entire starfield.
                let throttle_bits = throttler.count_ones();
                let throttle_factor = (1.0 - throttle_bits as f32 * 0.20).max(0.1);

                // Thermal headroom: thm_limits is the configured trip point in °C.
                // headroom_factor shrinks toward 0 as temp approaches the limit,
                // biasing hue toward red even before throttle fires.
                let thm_limit: f32 = smbus
                    .and_then(|s| s.thm_limits.as_deref())
                    .and_then(|v| crate::models::telemetry::parse_hex_or_dec(v))
                    .map(|v| v as f32)
                    .unwrap_or(95.0);
                // headroom 0..1: 1.0 = cool, 0.0 = at the limit
                let headroom = ((thm_limit - temp) / thm_limit.max(1.0)).clamp(0.0, 1.0);
                // Hue anchor shifts toward red (0°) as headroom shrinks.
                // Normal: temp_to_hue gives 180°(cold)→0°(hot).
                // Near-limit: pull an additional 40° toward red.
                let hue_penalty = (1.0 - headroom) * 40.0;

                // Brightness from power (relative to baseline), modulated by throttle.
                let power_change = self.baseline.power_change(star.device_idx, power);
                let base_brightness =
                    (0.3 + power_change.max(0.0).min(1.0) * 0.7) * throttle_factor;

                // Multi-frequency twinkling — faster when throttled (nervous flicker).
                let current_change = self.baseline.current_change(star.device_idx, current);
                let twinkle_speed = 0.1
                    + current_change.max(0.0).min(1.0) * 0.3
                    + if throttler > 0 { 0.15 } else { 0.0 };
                star.phase += twinkle_speed;
                star.phase2 += twinkle_speed * 1.7;

                let twinkle1 = (star.phase.sin() * 0.5 + 0.5) * 0.15;
                let twinkle2 = (star.phase2.cos() * 0.5 + 0.5) * 0.10;
                let twinkle = twinkle1 + twinkle2;

                // Sparkle effect — occasional bright flash on power spikes.
                if power_change > 0.7
                    && (star.sparkle == 0.0 || star.sparkle > 0.95)
                    && (self.frame + star.core_idx as u32) % 50 == 0
                {
                    star.sparkle = 1.0;
                }
                if star.sparkle > 0.0 {
                    star.sparkle -= 0.05;
                    if star.sparkle < 0.0 {
                        star.sparkle = 0.0;
                    }
                }

                star.brightness = (base_brightness + twinkle).clamp(0.0, 1.0);

                // Color: rainbow cycling with thermal headroom pulling toward red.
                use crate::animation::{hsv_to_rgb, temp_to_hue};
                let hue = ((temp_to_hue(temp) - hue_penalty).max(0.0)
                    + self.frame as f32 * 2.0
                    + star.core_idx as f32 * 2.7)
                    % 360.0;
                // Saturation drops slightly when throttling — gives a "washed out"
                // desaturated look that reads as "something is wrong".
                let sat = if throttler > 0 {
                    0.5 + headroom * 0.5
                } else {
                    1.0
                };
                star.color = hsv_to_rgb(hue, sat, 1.0);
            }
        }

        // Update memory planet activity
        for planet in &mut self.planets {
            if let Some(telem) = backend.telemetry(planet.device_idx) {
                let current = telem.current_a();
                let current_change = self.baseline.current_change(planet.device_idx, current);

                // Real per-channel GDDR block for this device, if the backend
                // has one (tt-smi >= 6.3.0). Fetched once and shared by both
                // the activity and the colour assignment below, so the two can
                // never disagree about which signal source a DDR planet is on.
                let gddr = backend
                    .smbus_telemetry(planet.device_idx)
                    .and_then(|s| s.gddr_telemetry.as_ref())
                    .filter(|g| !g.channels.is_empty());

                // Different memory levels respond to different metrics
                planet.activity = match planet.level {
                    0 => {
                        // L1: Responds to power (compute activity)
                        let power = telem.power_w();
                        let power_change = self.baseline.power_change(planet.device_idx, power);
                        power_change.max(0.0).min(1.0)
                    }
                    1 => {
                        // L2: Responds to current (memory traffic)
                        current_change.max(0.0).min(1.0)
                    }
                    2 => {
                        // DDR: prefer real per-channel gddr_telemetry state
                        // (tt-smi >= 6.3.0) over the generic power/current
                        // proxy — a harvested or administratively-disabled
                        // channel is definitionally idle,
                        // a BIST-failed channel is a fault (max activity, not
                        // a number derived from unrelated power draw), and a
                        // healthy channel's real temperature is a much better
                        // activity signal than "how busy is the whole board".
                        // Falls back to the generic formula only when the device
                        // has no per-channel data *at all*.
                        //
                        // The fallback is deliberately per-device, not
                        // per-channel: planets are spawned from
                        // `device.memory_channels()` (12 on Blackhole) while
                        // tt-smi only reports the 8 real channels, so a
                        // per-channel fallback left 8 planets driven by real
                        // telemetry sitting next to 4 driven by an unrelated
                        // power/current proxy — two incommensurable signals in
                        // one ring, on exactly the hardware this feature
                        // targets. A channel with no real counterpart is now
                        // rendered inert (like a harvested one) instead, so
                        // every DDR planet on a device with real GDDR data is
                        // either genuinely real-data-driven or uniformly
                        // marked "not tracked".
                        match gddr {
                            None => {
                                let power = telem.power_w();
                                let power_change =
                                    self.baseline.power_change(planet.device_idx, power);
                                ((power_change + current_change) / 2.0).max(0.0).min(1.0)
                            }
                            Some(g) => {
                                match g.channels.iter().find(|c| c.channel == planet.channel_idx) {
                                    Some(c) if c.harvested || !c.enabled => 0.0,
                                    Some(c) if !c.bist_pass => 1.0, // max activity: flag the fault
                                    Some(c) => {
                                        let avg_temp = match (c.temp_top, c.temp_bottom) {
                                            (Some(t), Some(b)) => (t + b) / 2.0,
                                            (Some(t), None) | (None, Some(t)) => t,
                                            (None, None) => 0.0,
                                        };
                                        (avg_temp / 90.0).clamp(0.0, 1.0)
                                    }
                                    // Beyond the set tt-smi reports: inert.
                                    None => 0.0,
                                }
                            }
                        }
                    }
                    _ => 0.0,
                };

                // Planets: each memory level cycles through its own hue range,
                // per-channel offset spreads the colours so they aren't all in sync.
                use crate::animation::hsv_to_rgb as _hsv;
                // DDR planets prefer real per-channel gddr_telemetry state for
                // their color too: harvested or administratively-disabled
                // channels — and, for the same reason as the activity above,
                // channels tt-smi doesn't report at all — go dim gray (nothing
                // to show), BIST-failed channels
                // flicker bright/dark red as a fault beacon, and everything
                // else (L1/L2 planets, and every DDR planet on a device with
                // no per-channel block at all) keeps the existing hue-cycling
                // look untouched.
                let ddr_gddr = if planet.level == 2 { gddr } else { None };
                if let Some(g) = ddr_gddr {
                    match g.channels.iter().find(|c| c.channel == planet.channel_idx) {
                        Some(c) if c.harvested || !c.enabled => {
                            planet.color = Color::Rgb(60, 60, 60)
                        }
                        Some(c) if !c.bist_pass => {
                            let flicker = (self.frame / 3) % 2 == 0;
                            planet.color = if flicker {
                                Color::Rgb(255, 60, 60)
                            } else {
                                Color::Rgb(90, 20, 20)
                            };
                        }
                        Some(_) => {
                            let planet_hue = (self.frame as f32 * 2.5
                                + planet.channel_idx as f32 * 30.0)
                                % 360.0;
                            let planet_value = 0.6 + planet.activity * 0.4;
                            planet.color = _hsv(planet_hue, 1.0, planet_value);
                        }
                        // Beyond the set tt-smi reports: same inert dim gray a
                        // harvested channel gets, never the hue-cycling look —
                        // that would read as live data it isn't.
                        None => planet.color = Color::Rgb(60, 60, 60),
                    }
                } else {
                    let planet_hue = match planet.level {
                        0 => {
                            (240.0 + self.frame as f32 * 1.5 + planet.channel_idx as f32 * 90.0)
                                % 360.0
                        }
                        1 => {
                            (120.0 + self.frame as f32 * 1.0 + planet.channel_idx as f32 * 45.0)
                                % 360.0
                        }
                        _ => (self.frame as f32 * 2.5 + planet.channel_idx as f32 * 30.0) % 360.0,
                    };
                    let planet_value = 0.6 + planet.activity * 0.4;
                    planet.color = _hsv(planet_hue, 1.0, planet_value);
                }

                // Orbital motion - planets orbit around device center
                // Different levels orbit at different speeds
                let orbit_speed = match planet.level {
                    0 => 0.05, // L1 fastest (inner ring)
                    1 => 0.03, // L2 medium
                    2 => 0.02, // DDR slowest (outer ring)
                    _ => 0.0,
                };

                // Speed increases with activity
                planet.angle += orbit_speed * (1.0 + planet.activity * 0.5);

                // Calculate new orbital position
                let device_spacing = self.width / backend.devices().len().max(1);
                let device_center_x = (planet.device_idx * device_spacing) + (device_spacing / 2);
                let device_center_y = self.height / 2;

                let px = device_center_x as f32 + planet.angle.cos() * planet.radius;
                let py = device_center_y as f32 + planet.angle.sin() * planet.radius;

                if px >= 0.0
                    && (px as usize) < self.width
                    && py >= 0.0
                    && (py as usize) < self.height
                {
                    planet.x = px as usize;
                    planet.y = py as usize;
                }

                // Pulsing animation for size variation (visual effect)
                planet.pulse += 0.08 * (1.0 + planet.activity);
            }
        }

        // Update data flow streams using sync_score (geometric mean of both
        // chips' activity).  This remains non-zero even when both chips are
        // equally loaded — solving the "equal-load streams go dark" problem
        // that the old abs(power_diff)/50 formula had.
        for stream in &mut self.streams {
            let act_a = *self
                .device_activity
                .get(&stream.from_device)
                .unwrap_or(&0.0);
            let act_b = *self.device_activity.get(&stream.to_device).unwrap_or(&0.0);

            stream.intensity = sync_score(act_a, act_b, stream.intra_board);

            // Animate flow offset.
            stream.offset = (stream.offset + 0.1) % 1.0;
        }

        // Increment animation frame
        self.frame = self.frame.wrapping_add(1);
    }

    /// Render the starfield to Ratatui Lines
    ///
    /// Returns a vector of Ratatui Line structures with proper styling.
    /// Galaxy fleet renderer — active when device_count >= 32.
    ///
    /// Renders chips as individual stars (`·∘○◉●`) coloured by temperature and
    /// animated by power, set against a sparse starfield background.  The result
    /// looks like a galaxy cluster: each Tenstorrent chip is a star whose colour
    /// and brightness encode its current thermal/power state.
    ///
    /// Layout: chips are arranged in a tight grid with 2-char spacing (star + gap)
    /// so the field fills the width.  Remaining rows are filled with faint
    /// background stars for depth.
    fn render_fleet_heatmap(&self) -> Vec<ratatui::text::Line<'static>> {
        use crate::animation::common::temp_to_hue;
        use crate::animation::hsv_to_rgb;
        use ratatui::style::{Modifier, Style};
        use ratatui::text::{Line, Span};

        // Star characters in ascending brightness order.
        const STAR_CHARS: [char; 5] = ['·', '∘', '○', '◉', '●'];

        // Each chip cell = 2 chars: the star glyph + 1 space gap.
        let cell_w: usize = 2;
        let chips_per_row = (self.width / cell_w).max(1);
        let chip_rows = (self.device_count + chips_per_row - 1) / chips_per_row;

        // Vertical centering: place chip grid in the upper two-thirds.
        let top_pad = (self.height.saturating_sub(chip_rows + 2)) / 3;
        let _grid_end = top_pad + chip_rows + 1; // +1 for header row

        let mut lines: Vec<ratatui::text::Line<'static>> = Vec::with_capacity(self.height);

        for row in 0..self.height {
            // ── Header row (one row above chip grid) ───────────────────────
            // Suppressed when chrome is off (embedded in Arcade) so the
            // composite view owns the single shared telemetry strip; the row
            // then falls through to a background nebula row, preserving height.
            if row == top_pad && top_pad > 0 && self.chrome {
                let label = format!(
                    " ✦ Galaxy Fleet — {} chips · ·=cool ●=active ● =hot ",
                    self.device_count
                );
                lines.push(Line::from(vec![Span::styled(
                    label,
                    Style::default()
                        .fg(colors::rgb(79, 209, 197))
                        .add_modifier(Modifier::BOLD),
                )]));
                continue;
            }

            let chip_row = row.wrapping_sub(top_pad + 1);
            if row <= top_pad || chip_row >= chip_rows {
                // ── Background nebula rows ──────────────────────────────────
                // Scatter faint background stars pseudo-randomly for depth.
                let mut spans: Vec<Span<'static>> = Vec::new();
                let mut text = String::with_capacity(self.width);
                for col in 0..self.width {
                    // Hash position + frame for a slowly drifting starfield.
                    let seed = ((col * 7 + row * 13 + self.frame as usize * 3)
                        .wrapping_mul(2654435761))
                        & 0xFFFF;
                    let ch = if seed < 80 { '·' } else { ' ' };
                    text.push(ch);
                }
                spans.push(Span::styled(
                    text,
                    Style::default().fg(colors::rgb(30, 30, 60)),
                ));
                lines.push(Line::from(spans));
                continue;
            }

            // ── Chip row ────────────────────────────────────────────────────
            let mut spans: Vec<Span<'static>> = Vec::new();
            for col in 0..chips_per_row {
                let dev_idx = chip_row * chips_per_row + col;
                if dev_idx >= self.device_count {
                    // Pad remainder with faint background stars.
                    let seed = ((col * 11 + row * 17 + self.frame as usize)
                        .wrapping_mul(2654435761))
                        & 0xFFFF;
                    let ch = if seed < 100 { '·' } else { ' ' };
                    spans.push(Span::styled(
                        format!("{} ", ch),
                        Style::default().fg(colors::rgb(30, 30, 60)),
                    ));
                    continue;
                }

                let temp = *self.device_temps.get(&dev_idx).unwrap_or(&45.0);
                let power = *self.device_powers.get(&dev_idx).unwrap_or(&0.0);
                let act = *self.device_activity.get(&dev_idx).unwrap_or(&0.0);

                // Twinkle: use frame + device_idx to give each star a different phase.
                let phase = (self.frame as f32 * 0.15 + dev_idx as f32 * 0.7).sin();
                let twinkle = phase * 0.15; // ±15% brightness jitter
                let brightness = (act + 0.4 + twinkle).clamp(0.0, 1.0);

                // Pick star character by brightness.
                let star_idx = (brightness * (STAR_CHARS.len() - 1) as f32) as usize;
                let star_ch = STAR_CHARS[star_idx.min(STAR_CHARS.len() - 1)];

                // Colour via temperature hue (same ramp as portraits).
                let hue = temp_to_hue(temp);
                let sat = 0.7 + act * 0.3;
                let val = 0.5 + (power / 150.0_f32).min(0.5);
                let color = hsv_to_rgb(hue, sat.min(1.0), val.min(1.0));

                let modifier = if act > 0.7 {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                };
                spans.push(Span::styled(
                    format!("{} ", star_ch),
                    Style::default().fg(color).add_modifier(modifier),
                ));
            }
            lines.push(Line::from(spans));
        }

        lines
    }

    pub fn render(&self) -> Vec<ratatui::text::Line<'static>> {
        use ratatui::style::Style;
        use ratatui::text::{Line, Span};

        // Galaxy fleet mode: 32+ devices switch to a macro galaxy heat-map view.
        if self.device_count >= 32 {
            return self.render_fleet_heatmap();
        }

        // Create blank canvas with explicit black background
        let mut canvas: Vec<Vec<(char, Color)>> =
            vec![vec![(' ', colors::rgb(0, 0, 0)); self.width]; self.height];

        // Render stars
        for star in &self.stars {
            if star.x < self.width && star.y < self.height {
                canvas[star.y][star.x] = (star.get_char(), star.color);
            }
        }

        // Render memory planets
        for planet in &self.planets {
            if planet.x < self.width && planet.y < self.height {
                canvas[planet.y][planet.x] = (planet.get_char(), planet.color);
            }
        }

        // Render data streams using topology-aware thresholds and colors.
        //
        // Intra-board (─): always-on floor of 0.2 from sync_score, use board hue.
        // Inter-board (═): visible when intensity ≥ 0.05, use rainbow sweep.
        for stream in &self.streams {
            // Threshold: intra-board always shown (floor ≥ 0.2), inter-board only when active.
            let threshold = if stream.intra_board { 0.05 } else { 0.05 };
            if stream.intensity < threshold {
                continue;
            }
            let show = ((stream.offset * 10.0) as u32 + self.frame / 2) % 3 == 0;
            if show && stream.x < self.width && stream.y < self.height {
                use crate::animation::hsv_to_rgb;
                let stream_hue = if stream.intra_board {
                    // Use the board hue for intra-board links.
                    self.board_topology
                        .as_ref()
                        .map(|t| t.board_hue(stream.from_device))
                        .unwrap_or(200.0)
                } else {
                    // Full rainbow sweep for inter-board links.
                    (self.frame as f32 * 3.0 + stream.offset * 360.0) % 360.0
                };
                let value = 0.5 + stream.intensity * 0.5;
                let stream_color = hsv_to_rgb(stream_hue, 0.9, value);
                canvas[stream.y][stream.x] = (stream.get_char(), stream_color);
            }
        }

        // Convert canvas to Ratatui Lines with Spans
        let mut lines = Vec::new();

        for row in canvas {
            let mut spans = Vec::new();
            let mut current_text = String::new();
            let mut current_color = colors::rgb(0, 0, 0);

            for (ch, color) in row {
                if color != current_color {
                    // Push accumulated text with previous color
                    if !current_text.is_empty() {
                        // Only set foreground color - let widget background show through
                        spans.push(Span::styled(
                            current_text.clone(),
                            Style::default().fg(current_color),
                        ));
                        current_text.clear();
                    }
                    current_color = color;
                }
                current_text.push(ch);
            }

            // Push final span
            if !current_text.is_empty() {
                // Only set foreground color - let widget background show through
                spans.push(Span::styled(
                    current_text,
                    Style::default().fg(current_color),
                ));
            }

            lines.push(Line::from(spans));
        }

        lines
    }

    /// Render the starfield to a TerminalGrid (for GUI)
    ///
    /// This method produces the same visual output as render() but writes to
    /// a TerminalGrid instead of Ratatui Lines, allowing the GUI to display
    /// the same terminal-style visualization.
    #[cfg(feature = "gui")]
    pub fn render_to_grid(&self, grid: &mut crate::ui::gui::TerminalGrid) {
        use iced::Color as IcedColor;

        // Helper function to convert Ratatui Color to Iced Color
        fn ratatui_to_iced(color: Color) -> IcedColor {
            match color {
                Color::Rgb(r, g, b) => {
                    IcedColor::from_rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
                }
                Color::Indexed(_) => IcedColor::from_rgb(0.8, 0.8, 0.8),
                Color::Reset => IcedColor::from_rgb(0.8, 0.8, 0.8),
                _ => IcedColor::from_rgb(0.8, 0.8, 0.8),
            }
        }

        // Clear the grid
        grid.clear();

        // Create internal canvas (same as render())
        let mut canvas: Vec<Vec<(char, Color)>> =
            vec![vec![(' ', colors::text_secondary()); self.width]; self.height];

        // Render stars
        for star in &self.stars {
            if star.x < self.width && star.y < self.height {
                canvas[star.y][star.x] = (star.get_char(), star.color);
            }
        }

        // Render memory planets
        for planet in &self.planets {
            if planet.x < self.width && planet.y < self.height {
                canvas[planet.y][planet.x] = (planet.get_char(), planet.color);
            }
        }

        // Render data streams (topology-aware, same logic as render()).
        for stream in &self.streams {
            if stream.intensity < 0.05 {
                continue;
            }
            let show = ((stream.offset * 10.0) as u32 + self.frame / 2) % 3 == 0;
            if show && stream.x < self.width && stream.y < self.height {
                use crate::animation::hsv_to_rgb;
                let stream_hue = if stream.intra_board {
                    self.board_topology
                        .as_ref()
                        .map(|t| t.board_hue(stream.from_device))
                        .unwrap_or(200.0)
                } else {
                    (self.frame as f32 * 3.0 + stream.offset * 360.0) % 360.0
                };
                canvas[stream.y][stream.x] = (stream.get_char(), hsv_to_rgb(stream_hue, 0.9, 0.8));
            }
        }

        // Write canvas to terminal grid
        for (row_idx, row) in canvas.iter().enumerate() {
            if row_idx >= grid.height() {
                break;
            }
            for (col_idx, &(ch, color)) in row.iter().enumerate() {
                if col_idx >= grid.width() {
                    break;
                }
                let iced_color = ratatui_to_iced(color);
                grid.set_char(row_idx, col_idx, ch, iced_color);
            }
        }
    }

    /// Get baseline learning status message
    pub fn baseline_status(&self) -> String {
        if self.baseline.is_established() {
            "BASELINE ESTABLISHED".to_string()
        } else {
            let progress = (self.baseline.progress() * 20.0) as usize;
            format!("LEARNING BASELINE ({}/20)", progress)
        }
    }

    /// Check if baseline is established
    pub fn is_baseline_established(&self) -> bool {
        self.baseline.is_established()
    }

    /// Resize starfield display
    pub fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        // Stars/planets will be reinitialized on next device update
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_starfield_creation() {
        let starfield = HardwareStarfield::new(80, 24);
        assert_eq!(starfield.stars.len(), 0); // No stars until initialized
        assert_eq!(starfield.width, 80);
        assert_eq!(starfield.height, 24);
    }

    #[test]
    fn test_star_brightness_char() {
        let mut star = Star {
            x: 0,
            y: 0,
            device_idx: 0,
            core_idx: 0,
            brightness: 0.0,
            color: colors::primary(),
            phase: 0.0,
            depth: 1.0,
            phase2: 0.0,
            sparkle: 0.0,
        };

        assert_eq!(star.get_char(), '·'); // Dimmest

        star.brightness = 0.5;
        assert_eq!(star.get_char(), '○'); // Medium

        star.brightness = 1.0;
        assert_eq!(star.get_char(), '●'); // Brightest
    }

    #[test]
    fn test_memory_planet_chars() {
        let l1_planet = MemoryPlanet {
            x: 0,
            y: 0,
            device_idx: 0,
            level: 0,
            channel_idx: 0,
            activity: 0.5,
            color: colors::info(),
            angle: 0.0,
            radius: 10.0,
            pulse: 0.0,
        };
        assert_eq!(l1_planet.get_char(), '◆'); // L1 diamond

        let l2_planet = MemoryPlanet {
            x: 0,
            y: 0,
            device_idx: 0,
            level: 1,
            channel_idx: 0,
            activity: 0.5,
            color: colors::warning(),
            angle: 0.0,
            radius: 10.0,
            pulse: 0.0,
        };
        assert_eq!(l2_planet.get_char(), '◇'); // L2 outline diamond
    }

    /// Backend wrapper that delegates every `TelemetryBackend`-required
    /// method to an inner `MockBackend`, but overrides `smbus_telemetry` for
    /// device 0 with a hand-built `gddr_telemetry` block: channel 0 is a
    /// healthy channel at a known high temperature (80°C top and bottom),
    /// and channel 1 is harvested. Mirrors the `NoSmbusBackend` wrapper
    /// pattern used in `memory_castle.rs`'s gddr_telemetry tests.
    struct CustomGddrBackend(crate::backend::mock::MockBackend);

    impl TelemetryBackend for CustomGddrBackend {
        fn init(&mut self) -> crate::error::BackendResult<()> {
            self.0.init()
        }
        fn update(&mut self) -> crate::error::BackendResult<()> {
            self.0.update()
        }
        fn devices(&self) -> &[crate::models::Device] {
            self.0.devices()
        }
        fn telemetry(&self, device_idx: usize) -> Option<&crate::models::Telemetry> {
            self.0.telemetry(device_idx)
        }
        fn smbus_telemetry(
            &self,
            device_idx: usize,
        ) -> Option<&crate::models::telemetry::SmbusTelemetry> {
            if device_idx == 0 {
                use std::sync::OnceLock;
                static SMBUS: OnceLock<crate::models::telemetry::SmbusTelemetry> = OnceLock::new();
                Some(SMBUS.get_or_init(|| {
                    use crate::models::telemetry::{GddrChannel, GddrTelemetry, SmbusTelemetry};
                    let channels = vec![
                        GddrChannel {
                            channel: 0,
                            harvested: false,
                            enabled: true,
                            training_pass: true,
                            bist_pass: true,
                            temp_top: Some(80.0),
                            temp_bottom: Some(80.0),
                            corr_rd: Some(0),
                            corr_wr: Some(0),
                            uncorr_rd: Some(0),
                            uncorr_wr: Some(0),
                        },
                        GddrChannel {
                            channel: 1,
                            harvested: true,
                            enabled: false,
                            training_pass: false,
                            bist_pass: true,
                            temp_top: None,
                            temp_bottom: None,
                            corr_rd: Some(0),
                            corr_wr: Some(0),
                            uncorr_rd: Some(0),
                            uncorr_wr: Some(0),
                        },
                    ];
                    SmbusTelemetry {
                        gddr_telemetry: Some(GddrTelemetry {
                            speed: Some("16G".to_string()),
                            max_temp: Some(80.0),
                            enabled_mask: Some(0b01),
                            channels,
                        }),
                        ..Default::default()
                    }
                }))
            } else {
                self.0.smbus_telemetry(device_idx)
            }
        }
        fn backend_info(&self) -> String {
            self.0.backend_info()
        }
    }

    #[test]
    fn ddr_planet_activity_reflects_real_channel_temp_when_present() {
        let mut inner = crate::backend::mock::MockBackend::new(1);
        inner.init().expect("mock backend init");
        let backend = CustomGddrBackend(inner);
        assert!(
            backend
                .smbus_telemetry(0)
                .and_then(|s| s.gddr_telemetry.as_ref())
                .is_some(),
            "test setup: wrapper must report real gddr_telemetry for device 0"
        );

        let mut starfield = HardwareStarfield::new(80, 24);
        starfield.initialize_from_devices(backend.devices());
        starfield.update_from_telemetry(&backend);

        let channel0_activity = starfield
            .planets
            .iter()
            .find(|p| p.device_idx == 0 && p.level == 2 && p.channel_idx == 0)
            .map(|p| p.activity)
            .expect("device 0 must have a level==2/channel_idx==0 DDR planet");
        let channel1_activity = starfield
            .planets
            .iter()
            .find(|p| p.device_idx == 0 && p.level == 2 && p.channel_idx == 1)
            .map(|p| p.activity)
            .expect("device 0 must have a level==2/channel_idx==1 DDR planet");

        assert!(
            channel0_activity > 0.85,
            "channel 0 at 80°C real temp should read as high activity (80/90 clamped); got {}",
            channel0_activity
        );
        assert_eq!(
            channel1_activity, 0.0,
            "channel 1 is harvested and must show zero activity regardless of power/current"
        );
    }

    /// Backend wrapper reporting a **sparse** `gddr_telemetry` block for
    /// device 2 (a Blackhole `p150` in the mock's Mixed scenario, so
    /// `memory_channels()` == 12): only channels 0 and 1 are described, both
    /// healthy. This is the real tt-smi shape — it reports 8 channels on
    /// hardware whose architecture declares 12 — reduced to the smallest
    /// fixture that exhibits it.
    struct SparseGddrBackend(crate::backend::mock::MockBackend);

    impl TelemetryBackend for SparseGddrBackend {
        fn init(&mut self) -> crate::error::BackendResult<()> {
            self.0.init()
        }
        fn update(&mut self) -> crate::error::BackendResult<()> {
            self.0.update()
        }
        fn devices(&self) -> &[crate::models::Device] {
            self.0.devices()
        }
        fn telemetry(&self, device_idx: usize) -> Option<&crate::models::Telemetry> {
            self.0.telemetry(device_idx)
        }
        fn smbus_telemetry(
            &self,
            device_idx: usize,
        ) -> Option<&crate::models::telemetry::SmbusTelemetry> {
            if device_idx == 2 {
                use std::sync::OnceLock;
                static SMBUS: OnceLock<crate::models::telemetry::SmbusTelemetry> = OnceLock::new();
                Some(SMBUS.get_or_init(|| {
                    use crate::models::telemetry::{GddrChannel, GddrTelemetry, SmbusTelemetry};
                    let healthy = |channel: usize| GddrChannel {
                        channel,
                        harvested: false,
                        enabled: true,
                        training_pass: true,
                        bist_pass: true,
                        temp_top: Some(70.0),
                        temp_bottom: Some(70.0),
                        ..Default::default()
                    };
                    SmbusTelemetry {
                        gddr_telemetry: Some(GddrTelemetry {
                            speed: Some("16G".to_string()),
                            max_temp: Some(70.0),
                            enabled_mask: Some(0b11),
                            channels: vec![healthy(0), healthy(1)],
                        }),
                        ..Default::default()
                    }
                }))
            } else {
                self.0.smbus_telemetry(device_idx)
            }
        }
        fn backend_info(&self) -> String {
            self.0.backend_info()
        }
    }

    /// A DDR planet whose channel id is beyond what tt-smi reports must NOT
    /// fall back to the generic power/current formula while its neighbours run
    /// on real per-channel telemetry — that mixes two incommensurable signals
    /// in one ring. It renders inert instead, exactly like a harvested channel.
    ///
    /// Colour is the discriminating assertion: the generic fallback's
    /// hue-cycling branch is `hsv(h, 1.0, >= 0.6)`, which is fully saturated
    /// and can never be the neutral gray used for "not tracked", whereas an
    /// activity of `0.0` alone would be ambiguous (the generic formula can
    /// legitimately produce 0.0 on an idle device).
    #[test]
    fn ddr_planet_beyond_reported_channels_is_inert_not_generic() {
        let mut inner = crate::backend::mock::MockBackend::new(3);
        inner.init().expect("mock backend init");
        let backend = SparseGddrBackend(inner);
        assert_eq!(
            backend.devices()[2].architecture,
            crate::models::Architecture::Blackhole,
            "test setup: device 2 must be a Blackhole so memory_channels() (12) \
             exceeds the 2 channels the fixture reports"
        );
        assert!(
            backend.devices()[2].memory_channels() > 2,
            "test setup: the architecture must declare more channels than the fixture reports"
        );

        let mut starfield = HardwareStarfield::new(120, 40);
        starfield.initialize_from_devices(backend.devices());
        // Several ticks so the adaptive baseline is past its learning phase and
        // the generic power/current formula would have something real to say.
        for _ in 0..25 {
            starfield.update_from_telemetry(&backend);
        }

        let planet = |channel_idx: usize| {
            starfield
                .planets
                .iter()
                .find(|p| p.device_idx == 2 && p.level == 2 && p.channel_idx == channel_idx)
                .unwrap_or_else(|| {
                    panic!("device 2 must have a DDR planet for channel {channel_idx}")
                })
        };

        // Channel 10: no tt-smi counterpart → inert, and unmistakably so.
        let untracked = planet(10);
        assert_eq!(
            untracked.activity, 0.0,
            "an unreported channel must not borrow the generic power/current activity"
        );
        assert_eq!(
            untracked.color,
            Color::Rgb(60, 60, 60),
            "an unreported channel must use the same neutral gray a harvested \
             channel gets, never the generic hue-cycling look"
        );

        // Channel 0 is really reported: the real-data path is untouched.
        let tracked = planet(0);
        assert!(
            tracked.activity > 0.7,
            "channel 0 at 70°C real temp should read as high activity (70/90); got {}",
            tracked.activity
        );
        assert_ne!(
            tracked.color,
            Color::Rgb(60, 60, 60),
            "a real, healthy channel must not render as 'not tracked'"
        );
    }

    /// Regression test: a channel that is `enabled: false` but NOT
    /// `harvested` (administratively disabled, not fused off — distinct
    /// states in `gddr_telemetry`) must render inert like a harvested
    /// channel, matching memory_flow.rs's `c.harvested || !c.enabled`
    /// convention. Starfield previously checked only `harvested`, so this
    /// state fell through to the healthy arm and rendered a live, glowing,
    /// temperature-tracking planet — inconsistent with Memory Flow showing
    /// the same hardware state as dark/inactive on the same telemetry tick.
    #[test]
    fn ddr_planet_inert_when_disabled_but_not_harvested() {
        struct DisabledNotHarvestedBackend(crate::backend::mock::MockBackend);
        impl crate::backend::TelemetryBackend for DisabledNotHarvestedBackend {
            fn init(&mut self) -> crate::error::BackendResult<()> {
                self.0.init()
            }
            fn update(&mut self) -> crate::error::BackendResult<()> {
                self.0.update()
            }
            fn devices(&self) -> &[crate::models::Device] {
                self.0.devices()
            }
            fn telemetry(&self, device_idx: usize) -> Option<&crate::models::Telemetry> {
                self.0.telemetry(device_idx)
            }
            fn smbus_telemetry(
                &self,
                device_idx: usize,
            ) -> Option<&crate::models::telemetry::SmbusTelemetry> {
                if device_idx == 0 {
                    use std::sync::OnceLock;
                    static SMBUS: OnceLock<crate::models::telemetry::SmbusTelemetry> =
                        OnceLock::new();
                    Some(SMBUS.get_or_init(|| {
                        use crate::models::telemetry::{
                            GddrChannel, GddrTelemetry, SmbusTelemetry,
                        };
                        SmbusTelemetry {
                            gddr_telemetry: Some(GddrTelemetry {
                                speed: Some("16G".to_string()),
                                max_temp: Some(90.0),
                                enabled_mask: Some(0b1),
                                channels: vec![GddrChannel {
                                    channel: 0,
                                    harvested: false, // NOT harvested
                                    enabled: false,   // administratively disabled
                                    training_pass: false,
                                    bist_pass: true,
                                    temp_top: Some(90.0), // would read as max activity if not caught
                                    temp_bottom: Some(90.0),
                                    ..Default::default()
                                }],
                            }),
                            ..Default::default()
                        }
                    }))
                } else {
                    self.0.smbus_telemetry(device_idx)
                }
            }
            fn backend_info(&self) -> String {
                self.0.backend_info()
            }
        }

        let mut inner = crate::backend::mock::MockBackend::new(1);
        inner.init().expect("mock backend init");
        let backend = DisabledNotHarvestedBackend(inner);

        let mut starfield = HardwareStarfield::new(120, 40);
        starfield.initialize_from_devices(backend.devices());
        for _ in 0..25 {
            starfield.update_from_telemetry(&backend);
        }

        let planet = starfield
            .planets
            .iter()
            .find(|p| p.device_idx == 0 && p.level == 2 && p.channel_idx == 0)
            .expect("device 0 must have a level==2/channel_idx==0 DDR planet");

        assert_eq!(
            planet.activity, 0.0,
            "a disabled-but-not-harvested channel must render inert, not driven by its (irrelevant) 90°C reading"
        );
        assert_eq!(
            planet.color,
            Color::Rgb(60, 60, 60),
            "a disabled-but-not-harvested channel must use the same neutral gray as a harvested one"
        );
    }

    #[test]
    fn test_data_stream_chars() {
        let intra = DataStream {
            x: 0,
            y: 0,
            from_device: 0,
            to_device: 1,
            intensity: 0.5,
            offset: 0.0,
            intra_board: true,
        };
        assert_eq!(intra.get_char(), '─');

        let inter = DataStream {
            x: 0,
            y: 0,
            from_device: 0,
            to_device: 2,
            intensity: 0.5,
            offset: 0.0,
            intra_board: false,
        };
        assert_eq!(inter.get_char(), '═');
    }
}
