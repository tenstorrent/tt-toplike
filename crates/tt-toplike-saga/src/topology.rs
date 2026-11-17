//! Floor specifications: the shape of one chip's dungeon floor, taken from
//! the chip's real tile grid.
//!
//! A [`FloorSpec`] says how big the chip's tile grid is, which die columns
//! are harvested (dead), how many GDDR vaults sit on each side and which
//! Ethernet ports get a portal. The maze builder ([`crate::maze`]) turns it
//! into a floor. This module only does the arithmetic from readings to a spec.
//!
//! # Portals and ports
//!
//! Each portal belongs to one Ethernet port (the party addendum, section
//! 8.2), so a hero can later cross the link behind it.
//! [`FloorSpec::portal_ports`] lists those ports, ascending:
//!
//! * **Blackhole:** the live ports among the low 12 (one per Ethernet cage).
//!   With no mask reading, ports 0 to 11, so an unread chip is not drawn as
//!   cut off. The count is the same popcount the spec held before ports
//!   existed, so a Blackhole floor's cells did not move when ports came in.
//! * **Every other architecture:** the live ports among the low 16 when the
//!   mask is known, else ports 0 and 1. Before ports existed such a floor
//!   always had two portals whatever its mask said, so a generic floor with
//!   a known mask can have a different count, and different cells, than it
//!   had in 0.17.0. The 16 is a cap that keeps the north edge readable. It
//!   is no hardware number: the bus mask is 64 bits wide, and this crate
//!   copies no port count for chips other than Blackhole.
//!
//! # Where the Blackhole numbers come from
//!
//! The saga crate cannot depend on the root crate, so the dimensions are
//! copied here. They come from the Die 3D view in the root crate. The root
//! crate's `src/animation/die3d/saga_floor_tests.rs` (ledger ruling R10)
//! computes each one from Die 3D's own tables and constants and compares it
//! with [`FloorSpec::for_chip`], so a change on either side fails there.
//!
//! * `BLACKHOLE_COLS` = 17 and `BLACKHOLE_ROWS` = 12. Copied from
//!   `src/animation/die3d/layout.rs:143` (test `blackhole_layout_is_17_by_12`),
//!   which counts the die x and die y values of the generated tile table
//!   `BH_TILES` (`src/animation/die3d/bh_layout_data.rs:15`). The table's die
//!   x runs 0 to 16 and die y runs 0 to 11. `src/animation/die3d/scene.rs:38`
//!   writes the same 17 as `BOARD_COLS = 17 + 2 * MARGIN_X` and
//!   `scene.rs:39` writes the 12 as `BOARD_BAND = 12 + 2`.
//! * `BLACKHOLE_VAULTS_PER_SIDE` = 4. `scene.rs:43` has
//!   `GDDR_ROWS: [i32; 4]`, four GDDR rows on each side of the die.
//!   `scene.rs:46-58` documents the layout: channels 0 to 3 sit on the west
//!   DRAM NoC column and channels 4 to 7 on the east one. The vertical order
//!   within a side is not documented, so the spec only counts vaults per
//!   side and says nothing about their order.
//! * `BLACKHOLE_ETH_PORTS` = 12. `scene.rs:41` has `ETH_CAGES: usize = 12`,
//!   one Ethernet cage per bit of the live-link mask.
//! * Tensix lives in die columns 1 to 14 and a clear bit `b` of
//!   `enabled_tensix_col` harvests die column `b + 1`
//!   (`src/animation/die3d/layout.rs:110-121`, `tensix_mask_to_die_cols`).
//!   That rule was measured on two chips: mask 0x3edf gave dead columns 6 and
//!   9, and 0x3fdd gave 2 and 6. The other bits follow by assumption.

use tt_toplike_bus::EntityKey;

/// Largest floor width the saga plan allows, in cells.
pub const MAX_COLS: u8 = 120;
/// Largest floor height the saga plan allows, in cells.
pub const MAX_ROWS: u8 = 60;

/// Die columns of a Blackhole tile grid.
const BLACKHOLE_COLS: u8 = 17;
/// Die rows of a Blackhole tile grid.
const BLACKHOLE_ROWS: u8 = 12;
/// GDDR vaults on each side of a Blackhole die.
const BLACKHOLE_VAULTS_PER_SIDE: u8 = 4;
/// Ethernet cages on a Blackhole die, one per live-link mask bit.
const BLACKHOLE_ETH_PORTS: u8 = 12;
/// The ports a non-Blackhole floor reads from a known mask: the low 16.
const GENERIC_ETH_PORTS: u8 = 16;
/// Tensix die columns of a Blackhole die (columns 1 to 14).
const BLACKHOLE_TENSIX_COLS: u8 = 14;

/// Size and features of the floor for a chip of unknown or non-Blackhole
/// architecture. These are plain defaults with no hardware source.
const GENERIC_COLS: u8 = 16;
const GENERIC_ROWS: u8 = 10;
const GENERIC_VAULTS_PER_SIDE: u8 = 2;
/// The ports of a non-Blackhole floor whose mask is not known.
const GENERIC_PORTAL_PORTS: [u8; 2] = [0, 1];

/// The chip family a floor is drawn for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    Blackhole,
    Wormhole,
    Grayskull,
    /// The architecture is not known.
    Generic,
}

/// The shape of one chip's floor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloorSpec {
    /// The chip this floor belongs to.
    pub chip: EntityKey,
    pub arch: Arch,
    /// Tile grid width, 1 to [`MAX_COLS`].
    pub cols: u8,
    /// Tile grid height, 1 to [`MAX_ROWS`].
    pub rows: u8,
    /// Harvested die columns, ascending, each below `cols`.
    pub dead_cols: Vec<u8>,
    /// Harvested die rows, ascending, each below `rows`.
    pub dead_rows: Vec<u8>,
    /// GDDR vaults on the west side (Blackhole channels 0 to 3).
    pub west_vaults: u8,
    /// GDDR vaults on the east side (Blackhole channels 4 to 7).
    pub east_vaults: u8,
    /// The Ethernet port of each portal, ascending, at most 64 ports (see
    /// "Portals and ports" in the module comment). The number of portals is
    /// its length. The maze places them west to east in this order.
    pub portal_ports: Vec<u8>,
}

/// The set bits of `mask` among its low `ports` bits, ascending.
fn live_ports(mask: u64, ports: u8) -> Vec<u8> {
    (0..ports).filter(|p| mask & (1u64 << p) != 0).collect()
}

impl FloorSpec {
    /// The floor for `chip` from its readings. Any reading may be missing.
    ///
    /// * `tensix_cols` is `toplike.enabled_tensix_col`. For Blackhole, a clear
    ///   bit `b` among the low 14 bits marks die column `b + 1` dead, the same
    ///   rule Die 3D uses. Higher bits are ignored. A missing value means no
    ///   dead columns, since nothing is known to be harvested.
    /// * `eth_live_mask` is `toplike.eth.live_mask`. A Blackhole floor gets
    ///   one portal per set bit among the low 12 (one per Ethernet cage),
    ///   bound to that port. A missing value gives all 12 cages, so an
    ///   unread chip is not drawn as cut off. Other architectures read the
    ///   low 16 bits, or get ports 0 and 1 with no reading (see "Portals and
    ///   ports" in the module comment).
    /// * `harvesting` is `toplike.harvesting_state`. Its bit layout is not
    ///   documented in `docs/bus.md` (which says only "register value") or in
    ///   `src/bus/signals.rs`, and Die 3D does not read it. The simplest
    ///   reading that follows those sources is to leave it out: harvesting
    ///   reaches the floor through `enabled_tensix_col`, so `dead_rows` stays
    ///   empty for every architecture. The argument is kept so a later task
    ///   can use it once its bits are known.
    ///
    /// Architectures other than Blackhole have no copied numbers, so they take
    /// the generic dimensions and no dead columns (the Tensix mask rule is
    /// Blackhole's). Their portals follow the mask as above. The `arch`
    /// value is still recorded.
    ///
    /// The function does not panic for any input. Every column and row index
    /// it returns is below `cols` and `rows`, and both sizes stay within
    /// [`MAX_COLS`] and [`MAX_ROWS`].
    pub fn for_chip(
        chip: &EntityKey,
        harvesting: Option<u64>,
        tensix_cols: Option<u64>,
        eth_live_mask: Option<u64>,
        arch: Arch,
    ) -> FloorSpec {
        // Not used yet; see the doc comment.
        let _ = harvesting;

        if arch != Arch::Blackhole {
            let mut spec = FloorSpec::generic(chip);
            spec.arch = arch;
            if let Some(mask) = eth_live_mask {
                spec.portal_ports = live_ports(mask, GENERIC_ETH_PORTS);
            }
            return spec;
        }

        let cols = BLACKHOLE_COLS.clamp(1, MAX_COLS);
        let rows = BLACKHOLE_ROWS.clamp(1, MAX_ROWS);

        // A clear bit among the low 14 is a harvested Tensix column. The die
        // column is the bit index plus one (column 0 is a DRAM column).
        let dead_cols = match tensix_cols {
            Some(mask) => (0..BLACKHOLE_TENSIX_COLS)
                .filter(|bit| mask & (1u64 << bit) == 0)
                .map(|bit| bit + 1)
                .filter(|&col| col < cols)
                .collect(),
            None => Vec::new(),
        };

        // One portal per live cage, bound to its port. No reading: every
        // cage.
        let portal_ports = live_ports(eth_live_mask.unwrap_or(u64::MAX), BLACKHOLE_ETH_PORTS);

        FloorSpec {
            chip: chip.clone(),
            arch,
            cols,
            rows,
            dead_cols,
            dead_rows: Vec::new(),
            west_vaults: BLACKHOLE_VAULTS_PER_SIDE,
            east_vaults: BLACKHOLE_VAULTS_PER_SIDE,
            portal_ports,
        }
    }

    /// The floor for a chip whose architecture is not known: default
    /// dimensions, nothing harvested, and portals on ports 0 and 1.
    pub fn generic(chip: &EntityKey) -> FloorSpec {
        FloorSpec {
            chip: chip.clone(),
            arch: Arch::Generic,
            cols: GENERIC_COLS.clamp(1, MAX_COLS),
            rows: GENERIC_ROWS.clamp(1, MAX_ROWS),
            dead_cols: Vec::new(),
            dead_rows: Vec::new(),
            west_vaults: GENERIC_VAULTS_PER_SIDE,
            east_vaults: GENERIC_VAULTS_PER_SIDE,
            portal_ports: GENERIC_PORTAL_PORTS.to_vec(),
        }
    }
}

#[cfg(test)]
#[path = "topology_tests.rs"]
mod topology_tests;
