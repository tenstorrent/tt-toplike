//! Tests for floors built from the chip topology.

use super::*;
use tt_toplike_bus::EntityKey;

fn chip() -> EntityKey {
    EntityKey::chip("0000:01:00.0")
}

#[test]
fn a_blackhole_floor_has_the_documented_vaults() {
    let spec = FloorSpec::for_chip(&chip(), None, None, None, Arch::Blackhole);
    assert_eq!(spec.arch, Arch::Blackhole);
    assert_eq!(spec.chip, chip());
    // Die 3D: 17 die columns by 12 die rows.
    assert_eq!((spec.cols, spec.rows), (17, 12));
    // GDDR channels 0 to 3 on the west, 4 to 7 on the east.
    assert_eq!((spec.west_vaults, spec.east_vaults), (4, 4));
    // No readings: every one of the 12 Ethernet cages counts as a portal.
    assert_eq!(spec.portals, 12);
    assert!(spec.dead_cols.is_empty() && spec.dead_rows.is_empty());
}

#[test]
fn portals_follow_the_live_ethernet_links() {
    let f = |mask| FloorSpec::for_chip(&chip(), None, None, Some(mask), Arch::Blackhole).portals;
    assert_eq!(f(0), 0);
    assert_eq!(f(0b1011), 3);
    // Bits above the 12 cages are ignored.
    assert_eq!(f(u64::MAX), 12);
    assert_eq!(f(1 << 40), 0);
}

#[test]
fn harvested_columns_become_dead_columns() {
    // Measured on hardware (see Die 3D): 0x3edf lacks die columns 6 and 9.
    let a = FloorSpec::for_chip(&chip(), None, Some(0x3edf), None, Arch::Blackhole);
    assert_eq!(a.dead_cols, vec![6, 9]);
    let b = FloorSpec::for_chip(&chip(), None, Some(0x3fdd), None, Arch::Blackhole);
    assert_eq!(b.dead_cols, vec![2, 6]);
    // All 14 Tensix columns enabled: none dead. Bits above 14 are ignored.
    let c = FloorSpec::for_chip(&chip(), None, Some(0x3fff), None, Arch::Blackhole);
    assert!(c.dead_cols.is_empty());
    let d = FloorSpec::for_chip(&chip(), None, Some(u64::MAX), None, Arch::Blackhole);
    assert!(d.dead_cols.is_empty());
    // Mask 0 kills every Tensix column.
    let z = FloorSpec::for_chip(&chip(), None, Some(0), None, Arch::Blackhole);
    assert_eq!(z.dead_cols, (1..=14).collect::<Vec<u8>>());
}

#[test]
fn odd_inputs_never_panic_and_stay_in_bounds() {
    for h in [None, Some(0), Some(u64::MAX), Some(1 << 63)] {
        for t in [None, Some(0), Some(u64::MAX), Some(1 << 63)] {
            for arch in [
                Arch::Blackhole,
                Arch::Wormhole,
                Arch::Grayskull,
                Arch::Generic,
            ] {
                let s = FloorSpec::for_chip(&chip(), h, t, Some(u64::MAX), arch);
                assert!((1..=120).contains(&s.cols) && (1..=60).contains(&s.rows));
                assert!(s.dead_cols.iter().all(|&c| c < s.cols));
                assert!(s.dead_rows.iter().all(|&r| r < s.rows));
            }
        }
    }
}

#[test]
fn an_unknown_chip_gets_a_generic_floor() {
    let g = FloorSpec::generic(&chip());
    assert_eq!(g.arch, Arch::Generic);
    assert!(g.dead_cols.is_empty() && g.dead_rows.is_empty());
    assert!(g.west_vaults >= 1 && g.east_vaults >= 1 && g.portals >= 1);
    // Other architectures keep their name but take the generic dimensions,
    // and a Blackhole column mask does not apply to them.
    let w = FloorSpec::for_chip(&chip(), None, Some(0), None, Arch::Wormhole);
    assert_eq!(w.arch, Arch::Wormhole);
    assert_eq!((w.cols, w.rows), (g.cols, g.rows));
    assert_eq!(
        (w.west_vaults, w.east_vaults, w.portals),
        (g.west_vaults, g.east_vaults, g.portals)
    );
    assert!(w.dead_cols.is_empty());
    // The generic floor is not the Blackhole floor.
    let bh = FloorSpec::for_chip(&chip(), None, None, None, Arch::Blackhole);
    assert_ne!((g.cols, g.rows), (bh.cols, bh.rows));
}
