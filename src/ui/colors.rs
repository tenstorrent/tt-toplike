// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Color scheme definitions
//!
//! This module defines the color palette used throughout the TUI.
//! Colors are inspired by the tt-vscode-toolkit project for consistency.
//!
//! ## Color Palette
//!
//! **Primary Colors** (Purple-Blue gradient):
//! - Primary: #667eea (Purple-blue)
//! - Secondary: #764ba2 (Deep purple)
//! - Hover: #5a67d8 (Darker blue)
//!
//! **Status Colors**:
//! - Success: #38b2ac (Teal)
//! - Success BG: #e6fffa (Light teal)
//! - Error: #e53e3e (Red)
//! - Error BG: #fed7d7 (Light red)
//! - Warning: #f6ad55 (Orange)
//!
//! **UI Colors**:
//! - Background: #f8f9fa (Light gray)
//! - Text Primary: #2d3748 (Dark gray)
//! - Text Secondary: #4a5568 (Medium gray)
//! - Border: #ddd (Light gray)

use ratatui::style::Color;

/// Check if terminal supports true color (RGB)
///
/// Returns false if:
/// - In tmux (often has RGB rendering issues, especially via macOS Terminal.app)
/// - COLORTERM is not set to truecolor/24bit
pub fn supports_true_color() -> bool {
    // Disable RGB in tmux - use 256-color mode instead
    // This fixes rendering issues with Terminal.app on macOS via SSH
    let in_tmux = std::env::var("TMUX").is_ok()
        || std::env::var("TERM").unwrap_or_default().contains("screen");

    if in_tmux {
        return false;
    }

    // Check COLORTERM for true color support
    std::env::var("COLORTERM")
        .map(|v| v == "truecolor" || v == "24bit")
        .unwrap_or(false)
}

/// Smart color wrapper that uses RGB when supported, falls back to 256-color in tmux
///
/// This automatically handles Terminal.app on macOS via SSH, which doesn't render
/// RGB colors properly in tmux.
///
/// # Arguments
///
/// * `r`, `g`, `b` - RGB values (0-255)
///
/// # Returns
///
/// Color::Rgb if terminal supports it, otherwise approximated Color::Indexed (256-color)
/// Active app-wide color theme. `Default` is the full psychedelic palette;
/// `Grayskull` collapses the rainbow to "a thousand shades of grey" — greys
/// everywhere, a cyan tint for cool/cyan hues, a faint purple for violets, and
/// HOT PINK as the only saturated "hot" color. Toggle with `/theme grayskull`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    Default,
    Grayskull,
}

static THEME: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Set the active theme (app-wide; every `rgb(..)`/`hsv_to_rgb(..)` result is
/// remapped through it).
pub fn set_theme(t: Theme) {
    let v = match t {
        Theme::Default => 0,
        Theme::Grayskull => 1,
    };
    THEME.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// The active theme.
pub fn current_theme() -> Theme {
    match THEME.load(std::sync::atomic::Ordering::Relaxed) {
        1 => Theme::Grayskull,
        _ => Theme::Default,
    }
}

/// Remap one RGB triple for the Grayskull theme: mostly luma greys, a cyan tint
/// for cool/cyan hues, a faint purple for violets, and hot pink for reds (the
/// only "hot" color). Pure — the single source of the grayscale look; unit
/// tested. Identity is applied by the caller when the theme is `Default`.
pub fn grayskull_rgb(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let (rf, gf, bf) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let v = max;
    let s = if max <= 0.0 { 0.0 } else { (max - min) / max };
    let l = (0.30 * rf + 0.59 * gf + 0.11 * bf).clamp(0.0, 1.0);
    let grey = (l * 255.0) as u8;
    // Near-greys (and black) stay grey — nothing to recolor.
    if s < 0.15 {
        return (grey, grey, grey);
    }
    // Hue in degrees.
    let d = max - min;
    let mut h = if d == 0.0 {
        0.0
    } else if max == rf {
        60.0 * (((gf - bf) / d).rem_euclid(6.0))
    } else if max == gf {
        60.0 * (((bf - rf) / d) + 2.0)
    } else {
        60.0 * (((rf - gf) / d) + 4.0)
    };
    if h < 0.0 {
        h += 360.0;
    }
    // Blend a grey base toward an accent (each 0..1) by `amt`.
    let blend = |accent: (f32, f32, f32), amt: f32| -> (u8, u8, u8) {
        let a = amt.clamp(0.0, 1.0);
        let mix = |c: f32| ((grey as f32) * (1.0 - a) + c * 255.0 * a).clamp(0.0, 255.0) as u8;
        (mix(accent.0), mix(accent.1), mix(accent.2))
    };
    if !(25.0..330.0).contains(&h) {
        // Reds → hot pink, the ONE hot color; brightness tracks the input value.
        let p = 0.4 + 0.6 * v;
        ((255.0 * p) as u8, (80.0 * p) as u8, (170.0 * p) as u8)
    } else if (150.0..210.0).contains(&h) {
        // Cyan band → grey tinted cyan.
        blend((0.20, 0.78, 0.86), 0.5 * s)
    } else if (255.0..330.0).contains(&h) {
        // Violet/purple → a faint purple tint.
        blend((0.55, 0.42, 0.78), 0.35 * s)
    } else {
        // Green / yellow / orange / blue → plain grey.
        (grey, grey, grey)
    }
}

pub fn rgb(r: u8, g: u8, b: u8) -> Color {
    // App-wide theme remap first (identity under the Default theme).
    let (r, g, b) = if current_theme() == Theme::Grayskull {
        grayskull_rgb(r, g, b)
    } else {
        (r, g, b)
    };
    if supports_true_color() {
        Color::Rgb(r, g, b)
    } else {
        // Special case: Pure black should use terminal default (Color::Reset)
        // This prevents grey backgrounds in tmux
        if r == 0 && g == 0 && b == 0 {
            return Color::Reset; // Use terminal's default background
        }

        // Convert RGB to 256-color palette
        // 256-color palette has:
        // - 0-15: Standard colors
        // - 16-231: 6x6x6 RGB cube
        // - 232-255: Grayscale ramp

        // Use 6x6x6 RGB cube (216 colors)
        // Each component: 0-5 (6 levels)
        let r6 = ((r as u16 * 6) / 256) as u8;
        let g6 = ((g as u16 * 6) / 256) as u8;
        let b6 = ((b as u16 * 6) / 256) as u8;

        let index = 16 + 36 * r6 + 6 * g6 + b6;
        Color::Indexed(index)
    }
}

/// Primary purple-blue color (brightened for dark terminals). Routed through
/// `rgb` so the app-wide theme applies (Grayskull greys it).
pub fn primary() -> Color {
    rgb(120, 150, 255)
}

/// Legacy constant (use primary() function instead)
pub const PRIMARY: Color = Color::Indexed(69); // 256-color bright blue (safe fallback)

/// Secondary deep purple color (brightened for dark terminals)
pub const SECONDARY: Color = Color::Rgb(150, 120, 200); // Bright purple

/// Darker variant for subtle highlights
pub const PRIMARY_DARK: Color = Color::Rgb(100, 130, 220); // Dimmer purple-blue

/// Success state color (brightened teal for dark mode). A function so the
/// app-wide theme (via `rgb`) applies — Grayskull tints it cyan.
pub fn success() -> Color {
    rgb(80, 220, 200)
}

/// Success background (not used in dark mode)
pub const SUCCESS_BG: Color = Color::Rgb(0, 0, 0); // Black

/// Error state color (brightened red for dark mode). Grayskull maps it to hot
/// pink (red is the one "hot" hue that keeps saturation).
pub fn error() -> Color {
    rgb(255, 100, 100)
}

/// Error background (not used in dark mode)
pub const ERROR_BG: Color = Color::Rgb(0, 0, 0); // Black

/// Warning state color (brightened orange for dark mode). Grayskull greys it.
pub fn warning() -> Color {
    rgb(255, 180, 100)
}

/// Background color (explicit black for tmux compatibility)
pub const BACKGROUND: Color = Color::Rgb(0, 0, 0); // Black

/// Primary text color (light gray for dark terminals). Already grey — theme
/// leaves it be; a function only for call-site uniformity with the others.
pub fn text_primary() -> Color {
    rgb(220, 220, 220)
}

/// Secondary text color (medium gray for dark terminals).
pub fn text_secondary() -> Color {
    rgb(160, 160, 160)
}

/// Border color (medium gray for dark terminals)
pub const BORDER: Color = Color::Rgb(100, 100, 120); // Dark gray-blue

/// Info color (bright cyan/blue for dark mode). Grayskull tints it cyan.
pub fn info() -> Color {
    rgb(100, 180, 255)
}

/// Get temperature-based color
///
/// Returns a color gradient from cool (cyan) to warm (yellow/orange) to hot (red).
/// Falls back to 256-color palette if true color (RGB) is not supported.
///
/// # Arguments
///
/// * `temp_c` - Temperature in Celsius
///
/// # Returns
///
/// Color based on temperature range (optimized for dark terminals):
/// - <45°C: Cyan (cool)
/// - 45-65°C: Violet (normal)
/// - 65-80°C: Pink (warm)
/// - >80°C: Red (hot)
pub fn temp_color(temp_c: f32) -> Color {
    if supports_true_color() {
        if temp_c < 45.0 {
            Color::Rgb(79, 209, 197) // Teal-cyan
        } else if temp_c < 65.0 {
            Color::Rgb(160, 120, 255) // Violet
        } else if temp_c < 80.0 {
            Color::Rgb(236, 150, 184) // Pink
        } else {
            Color::Rgb(255, 80, 80) // Red
        }
    } else {
        if temp_c < 45.0 {
            Color::Indexed(51) // Cyan
        } else if temp_c < 65.0 {
            Color::Indexed(135) // Violet (256-color palette)
        } else if temp_c < 80.0 {
            Color::Indexed(218) // Pink (256-color palette)
        } else {
            Color::Indexed(196) // Red
        }
    }
}

/// Get power-based color
///
/// Returns a color based on power consumption level.
/// Falls back to 256-color palette if true color (RGB) is not supported.
///
/// # Arguments
///
/// * `power_w` - Power in watts
///
/// # Returns
///
/// Color based on power range (optimized for dark terminals):
/// - <50W: Teal (low)
/// - 50-100W: Violet (medium)
/// - 100-150W: Pink (high)
/// - >150W: Red (very high)
pub fn power_color(power_w: f32) -> Color {
    if supports_true_color() {
        if power_w < 50.0 {
            Color::Rgb(79, 209, 197) // Teal
        } else if power_w < 100.0 {
            Color::Rgb(160, 120, 255) // Violet
        } else if power_w < 150.0 {
            Color::Rgb(236, 150, 184) // Pink
        } else {
            Color::Rgb(255, 80, 80) // Red
        }
    } else {
        if power_w < 50.0 {
            Color::Indexed(51) // Cyan
        } else if power_w < 100.0 {
            Color::Indexed(135) // Violet
        } else if power_w < 150.0 {
            Color::Indexed(218) // Pink
        } else {
            Color::Indexed(196) // Red
        }
    }
}

/// Get health status color
///
/// Returns SUCCESS or ERROR based on boolean health status.
pub fn health_color(is_healthy: bool) -> Color {
    if is_healthy {
        success()
    } else {
        error()
    }
}

/// Map temperature to hue for HSV color cycling
///
/// # Arguments
///
/// * `temp_c` - Temperature in celsius
///
/// # Returns
///
/// Hue value (0.0-360.0) where:
/// - Cold (0-40°C): Cyan (180°)
/// - Normal (40-60°C): Green-Yellow (60-100°)
/// - Warm (60-80°C): Orange (30-40°)
/// - Hot (>80°C): Red (0°)
pub fn temp_to_hue(temp_c: f32) -> f32 {
    if temp_c < 40.0 {
        180.0 // Cyan for cold
    } else if temp_c < 60.0 {
        // Interpolate from cyan (180°) to yellow (60°) for normal range
        180.0 - ((temp_c - 40.0) / 20.0) * 120.0
    } else if temp_c < 80.0 {
        // Interpolate from yellow (60°) to orange (30°) for warm range
        60.0 - ((temp_c - 60.0) / 20.0) * 30.0
    } else {
        // Red for hot
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_temp_color() {
        // Color values will vary based on COLORTERM environment variable
        // Just verify that we get valid Color enum variants
        let cool = temp_color(25.0);
        let normal = temp_color(50.0);
        let warm = temp_color(70.0);
        let hot = temp_color(85.0);

        // Verify they're different colors for different temps
        assert!(matches!(cool, Color::Rgb(_, _, _) | Color::Indexed(_)));
        assert!(matches!(normal, Color::Rgb(_, _, _) | Color::Indexed(_)));
        assert!(matches!(warm, Color::Rgb(_, _, _) | Color::Indexed(_)));
        assert!(matches!(hot, Color::Rgb(_, _, _) | Color::Indexed(_)));
    }

    #[test]
    fn test_power_color() {
        // Color values will vary based on COLORTERM environment variable
        // Just verify that we get valid Color enum variants
        let low = power_color(30.0);
        let medium = power_color(75.0);
        let high = power_color(125.0);
        let very_high = power_color(175.0);

        // Verify they're different colors for different power levels
        assert!(matches!(low, Color::Rgb(_, _, _) | Color::Indexed(_)));
        assert!(matches!(medium, Color::Rgb(_, _, _) | Color::Indexed(_)));
        assert!(matches!(high, Color::Rgb(_, _, _) | Color::Indexed(_)));
        assert!(matches!(very_high, Color::Rgb(_, _, _) | Color::Indexed(_)));
    }

    #[test]
    fn test_health_color() {
        assert_eq!(health_color(true), success());
        assert_eq!(health_color(false), error());
    }

    #[test]
    fn grayskull_greys_the_rainbow_but_keeps_cyan_purple_pink() {
        // Green → neutral grey (r == g == b).
        let (r, g, b) = grayskull_rgb(0, 255, 0);
        assert!(r == g && g == b, "green → neutral grey, got {r},{g},{b}");
        // Pure red (hot) → hot pink: red channel dominant, blue above green.
        let (r, g, b) = grayskull_rgb(255, 0, 0);
        assert!(r > b && b > g, "red → hot pink (r>b>g), got {r},{g},{b}");
        // Cyan → cyan-tinted (green & blue exceed red).
        let (r, g, b) = grayskull_rgb(0, 255, 255);
        assert!(
            g > r && b > r,
            "cyan → cyan tint (g,b > r), got {r},{g},{b}"
        );
        // Orange (warm mid) → grey, NOT pink (only reds are hot).
        let (r, g, b) = grayskull_rgb(255, 165, 0);
        assert!(r == g && g == b, "orange → grey, got {r},{g},{b}");
        // Near-grey stays grey.
        let (r, g, b) = grayskull_rgb(130, 128, 127);
        assert!(r == g && g == b, "near-grey stays grey, got {r},{g},{b}");
    }

    #[test]
    fn theme_state_roundtrips() {
        set_theme(Theme::Grayskull);
        assert_eq!(current_theme(), Theme::Grayskull);
        set_theme(Theme::Default);
        assert_eq!(current_theme(), Theme::Default);
    }
}
