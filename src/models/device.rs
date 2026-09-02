// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

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
            Architecture::Grayskull => (10, 12), // 120 cores
            Architecture::Wormhole => (8, 10),   // 80 cores
            Architecture::Blackhole => (14, 16), // 224 cores
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

    /// Firmware versions from tt-smi `firmwares` block (None on older tt-smi).
    pub firmwares: Option<crate::models::telemetry::FirmwaresInfo>,

    /// Thermal/power limits from tt-smi `limits` block (None on older tt-smi).
    pub limits: Option<crate::models::telemetry::DeviceLimits>,

    /// PCIe link speed (e.g. "16.0 GT/s"), from board_info.
    pub pcie_speed: Option<String>,

    /// PCIe link width (e.g. 16), from board_info.
    pub pcie_width: Option<u8>,

    /// Optional Tensix-grid override `(rows, cols)`.
    ///
    /// Real TT silicon leaves this `None` and the grid comes from `architecture`.
    /// The Host/CPU backend sets it so logical CPU cores map into a star grid
    /// (otherwise `Architecture::Unknown` yields a 0×0 grid and renders nothing).
    #[serde(default)]
    pub grid_override: Option<(usize, usize)>,

    /// Optional memory-channel-count override.
    ///
    /// `None` for TT silicon (count comes from `architecture`). The Host backend
    /// sets it to the number of synthesised DDR channels it reports.
    #[serde(default)]
    pub channels_override: Option<usize>,
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
            firmwares: None,
            limits: None,
            pcie_speed: None,
            pcie_width: None,
            grid_override: None,
            channels_override: None,
        }
    }

    /// Get human-readable device name
    ///
    /// Format: "Wormhole-0" or "Grayskull-1"
    pub fn name(&self) -> String {
        format!("{}-{}", self.architecture.name(), self.index)
    }

    /// Terse device label: architecture abbreviation + index, no separator.
    ///
    /// Format: "BH0", "WH2", "GS1". The canonical short label used wherever a
    /// device needs identifying in a tight space (per-device column headers,
    /// telemetry strips) — distinct from [`Device::name`]'s longer
    /// "Architecture-N" form, which reads better in prose.
    pub fn short_label(&self) -> String {
        format!("{}{}", self.architecture.abbrev(), self.index)
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

    /// Get number of memory channels for this device.
    ///
    /// Prefers `channels_override` (set by the Host backend) over the
    /// architecture default, so CPU "devices" report a usable channel count.
    pub fn memory_channels(&self) -> usize {
        self.channels_override
            .unwrap_or_else(|| self.architecture.memory_channels())
    }

    /// Get Tensix grid dimensions `(rows, cols)` for this device.
    ///
    /// Prefers `grid_override` (set by the Host backend from CPU core count)
    /// over the architecture default.
    pub fn tensix_grid(&self) -> (usize, usize) {
        self.grid_override
            .unwrap_or_else(|| self.architecture.tensix_grid())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_architecture_detection() {
        assert_eq!(
            Architecture::from_board_type("e75"),
            Architecture::Grayskull
        );
        assert_eq!(
            Architecture::from_board_type("e150"),
            Architecture::Grayskull
        );
        assert_eq!(
            Architecture::from_board_type("n150"),
            Architecture::Wormhole
        );
        assert_eq!(
            Architecture::from_board_type("n300"),
            Architecture::Wormhole
        );
        assert_eq!(
            Architecture::from_board_type("p150"),
            Architecture::Blackhole
        );
        assert_eq!(
            Architecture::from_board_type("p300"),
            Architecture::Blackhole
        );
        assert_eq!(
            Architecture::from_board_type("unknown"),
            Architecture::Unknown
        );
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

    #[test]
    fn short_label_is_arch_abbrev_plus_index_no_separator() {
        let wh = Device::new(2, "n150".to_string(), "0000:01:00.0".to_string(), "".into());
        assert_eq!(wh.short_label(), "WH2");

        let bh = Device::new(
            0,
            "p150a".to_string(),
            "0000:02:00.0".to_string(),
            "".into(),
        );
        assert_eq!(bh.short_label(), "BH0");

        let gs = Device::new(11, "e75".to_string(), "0000:03:00.0".to_string(), "".into());
        assert_eq!(gs.short_label(), "GS11");
    }

    #[test]
    fn test_device_new_fields_default_none() {
        let d = Device::new(
            0,
            "p150a".to_string(),
            "0000:01:00.0".to_string(),
            "(0,0)".to_string(),
        );
        assert!(d.firmwares.is_none());
        assert!(d.limits.is_none());
        assert!(d.pcie_speed.is_none());
        assert!(d.pcie_width.is_none());
    }
}
