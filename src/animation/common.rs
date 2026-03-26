//! Common utilities for psychedelic visualizations
//!
//! Shared functions for:
//! - Color space conversions (HSV → RGB)
//! - Character intensity mapping (value → ANSI chars)
//! - Animation helpers (lerp, easing, phase wrapping)
//! - ARC health rendering

use ratatui::style::Color;
use crate::ui::colors;

/// Convert HSV color space to RGB
///
/// # Arguments
///
/// * `h` - Hue (0.0-360.0 degrees)
/// * `s` - Saturation (0.0-1.0)
/// * `v` - Value/Brightness (0.0-1.0)
///
/// # Returns
///
/// Ratatui Color with RGB values
///
/// # Example
///
/// ```
/// let red = hsv_to_rgb(0.0, 1.0, 1.0);
/// let cyan = hsv_to_rgb(180.0, 1.0, 1.0);
/// ```
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Color {
    let h = h % 360.0;
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;

    let (r, g, b) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };

    colors::rgb(
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

/// Convert RGB color space to HSV
///
/// # Arguments
///
/// * `r` - Red (0-255)
/// * `g` - Green (0-255)
/// * `b` - Blue (0-255)
///
/// # Returns
///
/// Tuple of (hue, saturation, value) where:
/// - hue: 0.0-360.0 degrees
/// - saturation: 0.0-1.0
/// - value: 0.0-1.0
pub fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let hue = if delta == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta) % 6.0)
    } else if max == g {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };

    let saturation = if max == 0.0 { 0.0 } else { delta / max };
    let value = max;

    (if hue < 0.0 { hue + 360.0 } else { hue }, saturation, value)
}

/// Map temperature to hue for psychedelic color cycling
///
/// Cold temps (0°C) → Cyan (180°)
/// Warm temps (50°C) → Yellow (60°)
/// Hot temps (100°C) → Red (0°)
pub fn temp_to_hue(temp_c: f32) -> f32 {
    // Map 0-100°C to 180-0° (cyan through yellow to red)
    180.0 - (temp_c.max(0.0).min(100.0) * 1.8)
}

/// Map numeric value to character intensity
///
/// Uses block drawing characters for smooth gradients:
/// `· ░ ▒ ▓ █`
///
/// # Arguments
///
/// * `value` - Normalized value (0.0-1.0)
/// * `chars` - Character gradient array (low to high intensity)
pub fn value_to_char_intensity(value: f32, chars: &[char]) -> char {
    let clamped = value.max(0.0).min(1.0);
    let index = (clamped * (chars.len() - 1) as f32) as usize;
    chars[index]
}

/// Standard ANSI block character gradient (low to high)
pub const BLOCK_CHARS: [char; 5] = ['·', '░', '▒', '▓', '█'];

/// Phosphor-style gradient for oscilloscope effects
pub const PHOSPHOR_CHARS: [char; 7] = ['·', '░', '▒', '▓', '█', '▓', '▒'];

/// Particle characters for data flow
pub const PARTICLE_CHARS: [char; 6] = ['·', '○', '◎', '◉', '●', '✦'];

// ========================================
// MEMORY CASTLE CHARACTER SETS (CP437 ANSI Art)
// ========================================

/// Castle door/gate characters (heavy box drawing)
/// Used for DDR channel gates in Greyskull castle theme
pub const DOOR_CHARS: [char; 4] = ['╔', '╗', '╚', '╝'];

/// Castle wall characters (box drawing)
pub const WALL_CHARS: [char; 4] = ['═', '║', '─', '│'];

/// Castle window characters (progression from empty to solid)
/// Used for L1 SRAM Tensix cores in castle towers
pub const WINDOW_CHARS: [char; 5] = ['□', '▫', '▪', '▪', '■'];

/// Shelf character for L2 cache (great hall shelves)
pub const SHELF_CHAR: char = '═';

/// Portal/wormhole characters (circular, swirling)
/// Used for Wormhole architecture portal nexus theme
pub const PORTAL_CHARS: [char; 6] = ['◯', '◎', '◉', '⊚', '⊛', '◉'];

/// Accretion disk characters (rotating phases)
/// Used for Blackhole architecture event horizon theme
pub const ACCRETION_CHARS: [char; 4] = ['◐', '◑', '◒', '◓'];

/// Singularity characters (gravitational intensity)
/// Used for Blackhole L1 SRAM cores near event horizon
pub const SINGULARITY_CHARS: [char; 5] = ['·', '∘', '○', '●', '◉'];

/// Map value to standard block character
pub fn value_to_block_char(value: f32) -> char {
    value_to_char_intensity(value, &BLOCK_CHARS)
}

/// Map value to castle window character (for Greyskull theme)
pub fn value_to_window_char(value: f32) -> char {
    value_to_char_intensity(value, &WINDOW_CHARS)
}

/// Map value to singularity character (for Blackhole theme)
pub fn value_to_singularity_char(value: f32) -> char {
    value_to_char_intensity(value, &SINGULARITY_CHARS)
}

/// Map value to portal character (for Wormhole theme)
pub fn value_to_portal_char(value: f32) -> char {
    value_to_char_intensity(value, &PORTAL_CHARS)
}

/// Linear interpolation between two values
///
/// # Arguments
///
/// * `a` - Start value
/// * `b` - End value
/// * `t` - Interpolation factor (0.0-1.0)
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.max(0.0).min(1.0)
}

/// Ease-in-out interpolation (smooth start and end)
///
/// Uses cubic easing: `t^2 * (3 - 2t)`
pub fn ease_in_out(t: f32) -> f32 {
    let t = t.max(0.0).min(1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Wrap phase angle to 0-2π range
pub fn wrap_phase(phase: f32) -> f32 {
    phase % (2.0 * std::f32::consts::PI)
}

/// 16-color ANSI palette (BBS-era aesthetic)
/// Note: Uses Color::Rgb directly since const arrays can't use runtime functions
pub const ANSI_PALETTE: [Color; 16] = [
    Color::Rgb(0, 0, 0),         // 0: Black
    Color::Rgb(255, 100, 100),   // 1: Red
    Color::Rgb(80, 220, 100),    // 2: Green
    Color::Rgb(255, 220, 100),   // 3: Yellow
    Color::Rgb(100, 150, 255),   // 4: Blue
    Color::Rgb(255, 100, 255),   // 5: Magenta
    Color::Rgb(100, 220, 220),   // 6: Cyan
    Color::Rgb(220, 220, 220),   // 7: White
    Color::Rgb(100, 100, 100),   // 8: Bright Black
    Color::Rgb(255, 150, 150),   // 9: Bright Red
    Color::Rgb(150, 255, 150),   // 10: Bright Green
    Color::Rgb(255, 255, 150),   // 11: Bright Yellow
    Color::Rgb(150, 200, 255),   // 12: Bright Blue
    Color::Rgb(255, 150, 255),   // 13: Bright Magenta
    Color::Rgb(150, 255, 255),   // 14: Bright Cyan
    Color::Rgb(255, 255, 255),   // 15: Bright White
];

/// Get ANSI palette color by index (wraps around if > 15)
pub fn ansi_color(index: usize) -> Color {
    ANSI_PALETTE[index % ANSI_PALETTE.len()]
}

/// Color cycling through ANSI palette
///
/// # Arguments
///
/// * `frame` - Current animation frame
/// * `speed` - Cycle speed (frames per color)
pub fn ansi_color_cycle(frame: u32, speed: u32) -> Color {
    let index = (frame / speed) as usize % 16;
    ANSI_PALETTE[index]
}

/// Render ARC health status bar for header
///
/// Format: "ARC: ●●●○ (3/4 OK)"
///
/// # Arguments
///
/// * `arc_health` - Vector of (device_idx, is_healthy) tuples
///
/// # Returns
///
/// Formatted string with colored health indicators
pub fn arc_health_header(arc_health: &[(usize, bool)]) -> String {
    let healthy_count = arc_health.iter().filter(|(_, h)| *h).count();
    let total_count = arc_health.len();

    let indicators: String = arc_health
        .iter()
        .map(|(_, healthy)| if *healthy { '●' } else { '○' })
        .collect();

    if healthy_count == total_count {
        format!("ARC: {} (All OK)", indicators)
    } else {
        format!("ARC: {} ({}/{} OK)", indicators, healthy_count, total_count)
    }
}

/// Get color for ARC health indicator
///
/// Solid colors for ARC health status
pub fn arc_health_color(is_healthy: bool, _frame: u32) -> Color {
    if is_healthy {
        colors::rgb(80, 220, 100)  // Bright green (healthy)
    } else {
        colors::rgb(255, 100, 100)  // Solid bright red (no blinking)
    }
}

/// Lissajous curve calculation for oscilloscope effects
///
/// # Arguments
///
/// * `t` - Time parameter (0.0-1.0)
/// * `a` - X frequency multiplier
/// * `b` - Y frequency multiplier
/// * `delta` - Phase offset
///
/// # Returns
///
/// (x, y) coordinates in range (-1.0 to 1.0)
pub fn lissajous(t: f32, a: f32, b: f32, delta: f32) -> (f32, f32) {
    let t = t * 2.0 * std::f32::consts::PI;
    let x = (a * t + delta).sin();
    let y = (b * t).sin();
    (x, y)
}

/// Spirograph-style pattern calculation
///
/// Creates complex geometric patterns from circular motion
pub fn spirograph(t: f32, r1: f32, r2: f32, d: f32) -> (f32, f32) {
    let t = t * 2.0 * std::f32::consts::PI;
    let ratio = (r1 - r2) / r2;
    let x = (r1 - r2) * t.cos() + d * (ratio * t).cos();
    let y = (r1 - r2) * t.sin() - d * (ratio * t).sin();
    (x / (r1 + r2), y / (r1 + r2))  // Normalize to -1.0..1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hsv_to_rgb() {
        // Red
        let red = hsv_to_rgb(0.0, 1.0, 1.0);
        if let Color::Rgb(r, g, b) = red {
            assert_eq!(r, 255);
            assert_eq!(g, 0);
            assert!(b < 10);
        }

        // Cyan
        let cyan = hsv_to_rgb(180.0, 1.0, 1.0);
        if let Color::Rgb(r, g, b) = cyan {
            assert_eq!(r, 0);
            assert!(g > 245);
            assert!(b > 245);
        }
    }

    #[test]
    fn test_temp_to_hue() {
        assert_eq!(temp_to_hue(0.0), 180.0);   // Cold = cyan
        assert_eq!(temp_to_hue(50.0), 90.0);   // Medium = yellow-green
        assert_eq!(temp_to_hue(100.0), 0.0);   // Hot = red
    }

    #[test]
    fn test_value_to_block_char() {
        assert_eq!(value_to_block_char(0.0), '·');
        assert_eq!(value_to_block_char(0.5), '▒');
        assert_eq!(value_to_block_char(1.0), '█');
    }

    #[test]
    fn test_lerp() {
        assert_eq!(lerp(0.0, 10.0, 0.0), 0.0);
        assert_eq!(lerp(0.0, 10.0, 0.5), 5.0);
        assert_eq!(lerp(0.0, 10.0, 1.0), 10.0);
    }

    #[test]
    fn test_arc_health_header() {
        let health = vec![(0, true), (1, true), (2, false), (3, true)];
        let header = arc_health_header(&health);
        assert!(header.contains("3/4"));
        assert!(header.contains("●●○●"));
    }

    #[test]
    fn test_lissajous() {
        let (x, y) = lissajous(0.0, 1.0, 1.0, 0.0);
        assert!(x.abs() < 0.01);  // Should be near origin at t=0
        assert!(y.abs() < 0.01);
    }

    #[test]
    fn test_castle_window_chars() {
        assert_eq!(value_to_window_char(0.0), '□');  // Empty window
        assert_eq!(value_to_window_char(1.0), '■');  // Solid window
    }

    #[test]
    fn test_singularity_chars() {
        assert_eq!(value_to_singularity_char(0.0), '·');  // Far from singularity
        assert_eq!(value_to_singularity_char(1.0), '◉');  // At event horizon
    }

    #[test]
    fn test_portal_chars() {
        assert_eq!(value_to_portal_char(0.0), '◯');  // Closed portal
        assert_eq!(value_to_portal_char(1.0), '◉');  // Open portal
    }
}
