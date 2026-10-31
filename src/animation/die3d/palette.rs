// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The Die 3D colours: a Grayskull-style palette of greys, lilacs and one hot
//! pink, and a full-colour one. `/theme` picks between them ([`for_theme`]).
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
    /// clouds scale it by one of three flat density bands (0.6, 0.8 or 1.0
    /// of it, see `clouds::BANDS`) and by an intensity that follows chip
    /// load (at most 0.75), so the brightest cloud is about as bright as an
    /// idle tile and the sky never competes with the dies.
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

/// The full-colour palette, for the app's default theme: the heat ramp runs from
/// cool blue through amber to a hot red-pink, and each kind of tile has its own
/// hue (green GDDR, orange Ethernet, cyan and magenta traffic). The slab stays
/// a near-black blue so the colours carry the scene.
pub const RAINBOW: Palette = Palette {
    tile: [44, 72, 190],
    warm: [255, 206, 64],
    hot: [255, 58, 112],
    dram: [64, 206, 126],
    eth: [255, 160, 58],
    mgmt: [128, 138, 160],
    slab: [14, 20, 36],
    noc0: [226, 92, 232],
    noc1: [64, 224, 244],
    packet_in: [84, 232, 252],
    packet_out: [255, 190, 84],
    white: [255, 255, 255],
    cloud: [58, 84, 140],
};

/// The palette a theme draws with: greys for Grayskull, full colour for the
/// default theme.
pub fn for_theme(theme: crate::ui::colors::Theme) -> &'static Palette {
    match theme {
        crate::ui::colors::Theme::Grayskull => &GRAYSKULL,
        crate::ui::colors::Theme::Default => &RAINBOW,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::colors::Theme;

    fn roles(p: &Palette) -> [[u8; 3]; 13] {
        [
            p.tile,
            p.warm,
            p.hot,
            p.dram,
            p.eth,
            p.mgmt,
            p.slab,
            p.noc0,
            p.noc1,
            p.packet_in,
            p.packet_out,
            p.white,
            p.cloud,
        ]
    }

    #[test]
    fn a_theme_picks_its_palette() {
        assert_eq!(for_theme(Theme::Grayskull).tile, GRAYSKULL.tile);
        assert_eq!(for_theme(Theme::Default).tile, RAINBOW.tile);
        assert_ne!(roles(&GRAYSKULL), roles(&RAINBOW));
    }

    /// The point of the rainbow palette is colour: its heat ramp and tile kinds
    /// must be clearly more saturated than the greys, or `/theme` would change
    /// nothing a viewer could see.
    #[test]
    fn the_rainbow_palette_is_colourful_and_the_grayskull_one_is_not() {
        let spread = |c: [u8; 3]| c.iter().max().unwrap() - c.iter().min().unwrap();
        let ramp = |p: &Palette| [p.tile, p.warm, p.hot, p.dram, p.eth, p.noc0, p.noc1];
        for c in ramp(&RAINBOW) {
            assert!(spread(c) >= 100, "{c:?} is not colourful");
        }
        // Distinct hues along the ramp: no two of them within 40 of each other.
        let rb = ramp(&RAINBOW);
        for (i, a) in rb.iter().enumerate() {
            for b in &rb[i + 1..] {
                let d: i32 = (0..3).map(|k| (a[k] as i32 - b[k] as i32).abs()).sum();
                assert!(d >= 40, "{a:?} and {b:?} are too alike");
            }
        }
        assert!(spread(GRAYSKULL.mgmt) < 40 && spread(GRAYSKULL.slab) < 40);
    }
}
