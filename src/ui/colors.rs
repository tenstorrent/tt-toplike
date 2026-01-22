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

/// Primary purple-blue color (brightened for dark terminals)
pub const PRIMARY: Color = Color::Rgb(120, 150, 255); // Bright purple-blue

/// Secondary deep purple color (brightened for dark terminals)
pub const SECONDARY: Color = Color::Rgb(150, 120, 200); // Bright purple

/// Darker variant for subtle highlights
pub const PRIMARY_DARK: Color = Color::Rgb(100, 130, 220); // Dimmer purple-blue

/// Success state color (brightened teal for dark mode)
pub const SUCCESS: Color = Color::Rgb(80, 220, 200); // Bright teal

/// Success background (not used in dark mode)
pub const SUCCESS_BG: Color = Color::Reset; // Transparent

/// Error state color (brightened red for dark mode)
pub const ERROR: Color = Color::Rgb(255, 100, 100); // Bright red

/// Error background (not used in dark mode)
pub const ERROR_BG: Color = Color::Reset; // Transparent

/// Warning state color (brightened orange for dark mode)
pub const WARNING: Color = Color::Rgb(255, 180, 100); // Bright orange

/// Background color (use terminal default)
pub const BACKGROUND: Color = Color::Reset; // Terminal background

/// Primary text color (light gray for dark terminals)
pub const TEXT_PRIMARY: Color = Color::Rgb(220, 220, 220); // Light gray

/// Secondary text color (medium gray for dark terminals)
pub const TEXT_SECONDARY: Color = Color::Rgb(160, 160, 160); // Medium gray

/// Border color (medium gray for dark terminals)
pub const BORDER: Color = Color::Rgb(100, 100, 120); // Dark gray-blue

/// Info color (bright cyan/blue for dark mode)
pub const INFO: Color = Color::Rgb(100, 180, 255); // Bright blue

/// Get temperature-based color
///
/// Returns a color gradient from cool (cyan) to warm (yellow/orange) to hot (red).
///
/// # Arguments
///
/// * `temp_c` - Temperature in Celsius
///
/// # Returns
///
/// Color based on temperature range (optimized for dark terminals):
/// - <45°C: Bright cyan (cool)
/// - 45-65°C: Bright green-yellow (normal)
/// - 65-80°C: Bright orange (warm)
/// - >80°C: Bright red (hot)
pub fn temp_color(temp_c: f32) -> Color {
    if temp_c < 45.0 {
        Color::Rgb(80, 220, 220)  // Bright cyan
    } else if temp_c < 65.0 {
        Color::Rgb(150, 220, 100)  // Bright green-yellow
    } else if temp_c < 80.0 {
        Color::Rgb(255, 180, 100)  // Bright orange
    } else {
        Color::Rgb(255, 100, 100)  // Bright red
    }
}

/// Get power-based color
///
/// Returns a color based on power consumption level.
///
/// # Arguments
///
/// * `power_w` - Power in watts
///
/// # Returns
///
/// Color based on power range (optimized for dark terminals):
/// - <50W: Bright teal (low)
/// - 50-100W: Bright blue (medium)
/// - 100-150W: Bright orange (high)
/// - >150W: Bright red (very high)
pub fn power_color(power_w: f32) -> Color {
    if power_w < 50.0 {
        Color::Rgb(80, 220, 200)  // Bright teal
    } else if power_w < 100.0 {
        Color::Rgb(100, 180, 255)  // Bright blue
    } else if power_w < 150.0 {
        Color::Rgb(255, 180, 100)  // Bright orange
    } else {
        Color::Rgb(255, 100, 100)  // Bright red
    }
}

/// Get health status color
///
/// Returns SUCCESS or ERROR based on boolean health status.
pub fn health_color(is_healthy: bool) -> Color {
    if is_healthy {
        SUCCESS
    } else {
        ERROR
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
        assert_eq!(temp_color(25.0), SUCCESS);  // Cool
        assert_eq!(temp_color(50.0), INFO);     // Normal
        assert_eq!(temp_color(70.0), WARNING);  // Warm
        assert_eq!(temp_color(85.0), ERROR);    // Hot
    }

    #[test]
    fn test_power_color() {
        assert_eq!(power_color(30.0), SUCCESS);   // Low
        assert_eq!(power_color(75.0), INFO);      // Medium
        assert_eq!(power_color(125.0), WARNING);  // High
        assert_eq!(power_color(175.0), ERROR);    // Very high
    }

    #[test]
    fn test_health_color() {
        assert_eq!(health_color(true), SUCCESS);
        assert_eq!(health_color(false), ERROR);
    }
}
