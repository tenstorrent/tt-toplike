// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The Die 3D colours: a Grayskull-style palette of greys, lilacs and one hot
//! pink.
//!
//! The project's Grayskull theme (`ui/colors.rs`, `grayskull_rgb`) is the
//! reference, but the scene does not route through `colors::rgb`: the view
//! shades and blends these colours itself, so it needs the raw triples. The
//! target terminals (Konsole, GNOME Terminal) often have a purple or
//! translucent background, so the cool indigo greys sit well on it and the
//! scene leaves most pixels unpainted (see `PixelBuf::covered`).
//!
//! The heat ramp runs `tile` (indigo grey) to `warm` (lilac) to `hot` (hot
//! pink), staying in the purple and pink range the whole way: no orange, no
//! green.

/// Every colour the scene paints with.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// An idle Tensix tile: a low-saturation indigo grey.
    pub tile: [u8; 3],
    /// Midpoint of the heat ramp: lilac.
    pub warm: [u8; 3],
    /// Top of the heat ramp: hot pink, the one saturated colour.
    pub hot: [u8; 3],
    /// GDDR blocks: a cool blue grey.
    pub dram: [u8; 3],
    /// Ethernet cages: pale lavender.
    pub eth: [u8; 3],
    /// Management tiles: a neutral grey.
    pub mgmt: [u8; 3],
    /// The slab under the die: dark aubergine grey.
    pub slab: [u8; 3],
    /// NoC 0 traffic: orchid.
    pub noc0: [u8; 3],
    /// NoC 1 traffic: pale cyan, the theme's cyan tint.
    pub noc1: [u8; 3],
    /// Packets arriving over PCIe.
    pub packet_in: [u8; 3],
    /// Packets leaving the chip: pale pink, so they differ from the cyan ones
    /// arriving.
    pub packet_out: [u8; 3],
    /// Packet heads.
    pub white: [u8; 3],
    /// The clouds drifting behind the die (`clouds`): a lilac grey. The
    /// clouds scale it by density and by chip load (at most 0.75), so the
    /// brightest cloud is about as bright as an idle tile and the sky never
    /// competes with the dies.
    pub cloud: [u8; 3],
}

/// The Grayskull-style palette.
pub const GRAYSKULL: Palette = Palette {
    tile: [58, 52, 78],
    warm: [168, 120, 210],
    hot: [255, 92, 170],
    dram: [150, 168, 190],
    eth: [196, 170, 230],
    mgmt: [112, 110, 128],
    slab: [26, 24, 38],
    noc0: [190, 120, 235],
    noc1: [120, 205, 225],
    packet_in: [120, 205, 225],
    packet_out: [255, 200, 225],
    white: [255, 255, 255],
    cloud: [74, 62, 104],
};
