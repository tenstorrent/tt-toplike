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
    // No readings: every one of the 12 Ethernet cages counts as a portal,
    // ports 0 to 11.
    assert_eq!(spec.portal_ports, (0..12).collect::<Vec<u8>>());
    assert!(spec.dead_cols.is_empty() && spec.dead_rows.is_empty());
}

#[test]
fn portal_ports_follow_the_live_mask() {
    // Blackhole: the live ports among the low 12, ascending.
    let bh = |mask| FloorSpec::for_chip(&chip(), None, None, mask, Arch::Blackhole).portal_ports;
    assert_eq!(bh(Some(0)), Vec::<u8>::new());
    assert_eq!(bh(Some(0b1011)), vec![0, 1, 3]);
    assert_eq!(bh(Some(0b1000_0000_0010)), vec![1, 11]);
    // Bits above the 12 cages are ignored.
    assert_eq!(bh(Some(u64::MAX)), (0..12).collect::<Vec<u8>>());
    assert_eq!(bh(Some(1 << 40)), Vec::<u8>::new());
    assert_eq!(bh(Some(1 << 12)), Vec::<u8>::new());
    // No reading: every cage.
    assert_eq!(bh(None), (0..12).collect::<Vec<u8>>());
    // The count is the popcount of the low 12 bits, as before ports.
    for mask in [0u64, 1, 0x5, 0xfff, 0x1000, 0xaaaa_aaaa_aaaa_aaaa, 0x0f0f] {
        assert_eq!(
            bh(Some(mask)).len() as u32,
            (mask & 0xfff).count_ones(),
            "mask {mask:#x}"
        );
    }

    // Generic (and every other non-Blackhole architecture): the live ports
    // among the low 16 when the mask is known, else ports 0 and 1.
    for arch in [Arch::Generic, Arch::Wormhole, Arch::Grayskull] {
        let g = |mask| FloorSpec::for_chip(&chip(), None, None, mask, arch).portal_ports;
        assert_eq!(g(None), vec![0, 1], "{arch:?}");
        assert_eq!(g(Some(0)), Vec::<u8>::new(), "{arch:?}");
        assert_eq!(g(Some(0b1011)), vec![0, 1, 3], "{arch:?}");
        assert_eq!(
            g(Some(0x1_8001)),
            vec![0, 15],
            "{arch:?}: bit 16 is ignored"
        );
        assert_eq!(g(Some(u64::MAX)), (0..16).collect::<Vec<u8>>(), "{arch:?}");
    }
    assert_eq!(FloorSpec::generic(&chip()).portal_ports, vec![0, 1]);
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
    assert!(g.west_vaults >= 1 && g.east_vaults >= 1 && !g.portal_ports.is_empty());
    // Other architectures keep their name but take the generic dimensions,
    // and a Blackhole column mask does not apply to them.
    let w = FloorSpec::for_chip(&chip(), None, Some(0), None, Arch::Wormhole);
    assert_eq!(w.arch, Arch::Wormhole);
    assert_eq!((w.cols, w.rows), (g.cols, g.rows));
    assert_eq!(
        (w.west_vaults, w.east_vaults, &w.portal_ports),
        (g.west_vaults, g.east_vaults, &g.portal_ports)
    );
    assert!(w.dead_cols.is_empty());
    // The generic floor is not the Blackhole floor.
    let bh = FloorSpec::for_chip(&chip(), None, None, None, Arch::Blackhole);
    assert_ne!((g.cols, g.rows), (bh.cols, bh.rows));
}
