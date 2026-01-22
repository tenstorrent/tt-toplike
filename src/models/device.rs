//! Device information and architecture types
//!
//! Defines the Device struct and Architecture enum for representing
//! Tenstorrent hardware devices across different generations.

use serde::{Deserialize, Serialize};

/// Tenstorrent device architecture
///
/// Each architecture has different characteristics:
/// - **Grayskull (GS)**: e75, e150 boards, 4 DDR channels, 10×12 Tensix grid
/// - **Wormhole (WH)**: n150, n300 boards, 8 DDR channels, 8×10 Tensix grid
/// - **Blackhole (BH)**: p150, p300 boards, 12 DDR channels, 14×16 Tensix grid
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Architecture {
    /// Grayskull architecture (e75, e150)
    Grayskull,
    /// Wormhole architecture (n150, n300)
    Wormhole,
    /// Blackhole architecture (p150, p300)
    Blackhole,
    /// Unknown architecture (fallback)
    Unknown,
}

impl Architecture {
    /// Get human-readable name for architecture
    pub fn name(&self) -> &'static str {
        match self {
            Architecture::Grayskull => "Grayskull",
            Architecture::Wormhole => "Wormhole",
            Architecture::Blackhole => "Blackhole",
            Architecture::Unknown => "Unknown",
        }
    }

    /// Get short abbreviation for architecture
    pub fn abbrev(&self) -> &'static str {
        match self {
            Architecture::Grayskull => "GS",
            Architecture::Wormhole => "WH",
            Architecture::Blackhole => "BH",
            Architecture::Unknown => "UK",
        }
    }

    /// Get number of DDR memory channels for this architecture
    pub fn memory_channels(&self) -> usize {
        match self {
            Architecture::Grayskull => 4,
            Architecture::Wormhole => 8,
            Architecture::Blackhole => 12,
            Architecture::Unknown => 0,
        }
    }

    /// Get Tensix core grid dimensions (rows, cols)
    pub fn tensix_grid(&self) -> (usize, usize) {
        match self {
            Architecture::Grayskull => (10, 12),  // 120 cores
            Architecture::Wormhole => (8, 10),    // 80 cores
            Architecture::Blackhole => (14, 16),  // 224 cores
            Architecture::Unknown => (0, 0),
        }
    }

    /// Detect architecture from board type string
    ///
    /// Board type patterns:
    /// - e75, e150 → Grayskull
    /// - n150, n300 → Wormhole
    /// - p150, p300 → Blackhole
    pub fn from_board_type(board_type: &str) -> Self {
        let board_type_lower = board_type.to_lowercase();

        if board_type_lower.contains("e75") || board_type_lower.contains("e150") {
            Architecture::Grayskull
        } else if board_type_lower.contains("n150") || board_type_lower.contains("n300") {
            Architecture::Wormhole
        } else if board_type_lower.contains("p150") || board_type_lower.contains("p300") {
            Architecture::Blackhole
        } else {
            Architecture::Unknown
        }
    }
}

/// Device information struct
///
/// Represents a single Tenstorrent device with its identifying information.
/// This is a lightweight proxy object that doesn't hold telemetry data directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    /// Device index (0-based)
    pub index: usize,

    /// Board type string (e.g., "n150", "e75", "p300")
    pub board_type: String,

    /// PCI bus ID (e.g., "0000:01:00.0")
    pub bus_id: String,

    /// Device architecture (detected from board_type)
    pub architecture: Architecture,

    /// Device coordinates (if part of multi-device system)
    /// Format: "(rack, shelf, chip)" or "(x, y)"
    pub coords: String,
}

impl Device {
    /// Create a new Device instance
    ///
    /// Automatically detects architecture from board_type string.
    pub fn new(index: usize, board_type: String, bus_id: String, coords: String) -> Self {
        let architecture = Architecture::from_board_type(&board_type);

        Device {
            index,
            board_type,
            bus_id,
            architecture,
            coords,
        }
    }

    /// Get human-readable device name
    ///
    /// Format: "Wormhole-0" or "Grayskull-1"
    pub fn name(&self) -> String {
        format!("{}-{}", self.architecture.name(), self.index)
    }

    /// Check if device is Grayskull architecture
    pub fn is_grayskull(&self) -> bool {
        self.architecture == Architecture::Grayskull
    }

    /// Check if device is Wormhole architecture
    pub fn is_wormhole(&self) -> bool {
        self.architecture == Architecture::Wormhole
    }

    /// Check if device is Blackhole architecture
    pub fn is_blackhole(&self) -> bool {
        self.architecture == Architecture::Blackhole
    }

    /// Get number of memory channels for this device
    pub fn memory_channels(&self) -> usize {
        self.architecture.memory_channels()
    }

    /// Get Tensix grid dimensions for this device
    pub fn tensix_grid(&self) -> (usize, usize) {
        self.architecture.tensix_grid()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_architecture_detection() {
        assert_eq!(Architecture::from_board_type("e75"), Architecture::Grayskull);
        assert_eq!(Architecture::from_board_type("e150"), Architecture::Grayskull);
        assert_eq!(Architecture::from_board_type("n150"), Architecture::Wormhole);
        assert_eq!(Architecture::from_board_type("n300"), Architecture::Wormhole);
        assert_eq!(Architecture::from_board_type("p150"), Architecture::Blackhole);
        assert_eq!(Architecture::from_board_type("p300"), Architecture::Blackhole);
        assert_eq!(Architecture::from_board_type("unknown"), Architecture::Unknown);
    }

    #[test]
    fn test_architecture_properties() {
        assert_eq!(Architecture::Grayskull.memory_channels(), 4);
        assert_eq!(Architecture::Wormhole.memory_channels(), 8);
        assert_eq!(Architecture::Blackhole.memory_channels(), 12);

        assert_eq!(Architecture::Grayskull.tensix_grid(), (10, 12));
        assert_eq!(Architecture::Wormhole.tensix_grid(), (8, 10));
        assert_eq!(Architecture::Blackhole.tensix_grid(), (14, 16));
    }

    #[test]
    fn test_device_creation() {
        let device = Device::new(
            0,
            "n150".to_string(),
            "0000:01:00.0".to_string(),
            "(0,0)".to_string(),
        );

        assert_eq!(device.index, 0);
        assert_eq!(device.architecture, Architecture::Wormhole);
        assert_eq!(device.name(), "Wormhole-0");
        assert!(device.is_wormhole());
        assert!(!device.is_grayskull());
    }
}
