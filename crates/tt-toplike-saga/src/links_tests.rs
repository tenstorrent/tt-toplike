//! Tests for the link table: which portals are bound to a peer, and which
//! of those are open.

use super::*;
use crate::event::Biome;
use crate::maze;
use crate::rng::Pcg32;
use crate::topology::{Arch, FloorSpec};
use std::sync::Arc;

fn chip(i: u8) -> EntityKey {
    EntityKey::chip(&format!("0000:{:02x}:00.0", i + 1))
}

fn port(chip_index: u8, port: u8) -> EntityKey {
    EntityKey::eth_port(&chip(chip_index), port)
}

/// A cause with a made-up ordinal; the table only stores it.
fn cause(ordinal: u64) -> Cause {
    Cause {
        entity: chip(0),
        signal: "test".to_string(),
        ordinal,
    }
}

/// A world with one Blackhole floor per entry of `masks`, on chips 0, 1,
/// ... in order. Each floor's portals follow its mask at birth.
fn world_of(masks: &[Option<u64>]) -> World {
    let chips: Vec<EntityKey> = (0..masks.len() as u8).map(chip).collect();
    let floors: Vec<maze::Floor> = chips
        .iter()
        .zip(masks)
        .enumerate()
        .map(|(i, (c, &mask))| {
            let spec = FloorSpec::for_chip(c, None, None, mask, Arch::Blackhole);
            maze::generate(&spec, 40 + i as u64, 0.0)
        })
        .collect();
    World {
        id: 1,
        seed: 40,
        born_tick: 1,
        chips,
        floors: Arc::from(floors),
        biome: Biome::Dungeon,
        sage_chips: Vec::new(),
    }
}

/// Links with every chip of `n` fully live (the low 12 ports).
fn all_live(n: u8) -> Links {
    let mut links = Links::new();
    for i in 0..n {
        links.observe_mask(&chip(i), 0xfff, cause(1));
    }
    links
}

#[test]
fn a_portal_without_a_peer_record_never_opens() {
    let world = world_of(&[Some(0xfff), Some(0xfff)]);
    let mut links = all_live(2);
    // Every port of both chips is live, and no port has a peer.
    for floor in 0..2 {
        for p in 0..64 {
            assert_eq!(links.open(&world, floor, p), None, "floor {floor} port {p}");
            assert_eq!(
                links.bound(&world, floor, p),
                None,
                "floor {floor} port {p}"
            );
        }
    }
    // A record for one pair binds that pair only.
    links.observe_peer(&port(0, 3), &port(1, 5), cause(2));
    assert_eq!(links.open(&world, 0, 3), Some((1, 5)));
    for p in (0..64).filter(|&p| p != 3) {
        assert_eq!(links.open(&world, 0, p), None, "port {p}");
    }
    for p in (0..64).filter(|&p| p != 5) {
        assert_eq!(links.open(&world, 1, p), None, "port {p}");
    }
    // A floor the world does not have opens nothing.
    assert_eq!(links.open(&world, 2, 3), None);
}

#[test]
fn a_portal_opens_only_while_both_ends_are_live() {
    let world = world_of(&[Some(0xfff), Some(0xfff)]);
    let mut links = Links::new();
    links.observe_peer(&port(0, 11), &port(1, 1), cause(1));
    // No mask seen yet for either chip: a chip whose mask was never seen
    // counts as not live (the addendum, section 8.2).
    assert_eq!(links.open(&world, 0, 11), None);
    assert_eq!(links.bound(&world, 0, 11), Some((1, 1)));
    links.observe_mask(&chip(0), 0xfff, cause(2));
    assert_eq!(links.open(&world, 0, 11), None, "the far mask is unseen");
    links.observe_mask(&chip(1), 0xfff, cause(3));
    // Both ends live: open in both directions.
    assert_eq!(links.open(&world, 0, 11), Some((1, 1)));
    assert_eq!(links.open(&world, 1, 1), Some((0, 11)));
    // The near end goes down.
    links.observe_mask(&chip(0), 0xfff & !(1 << 11), cause(4));
    assert!(!links.live(&chip(0), 11));
    assert_eq!(links.open(&world, 0, 11), None);
    assert_eq!(links.open(&world, 1, 1), None, "seen from the far side");
    // Back up, then the far end goes down.
    links.observe_mask(&chip(0), 0xfff, cause(5));
    assert_eq!(links.open(&world, 0, 11), Some((1, 1)));
    links.observe_mask(&chip(1), 0xfff & !(1 << 1), cause(6));
    assert!(links.live(&chip(0), 11));
    assert_eq!(links.open(&world, 0, 11), None, "the far end is dark");
    assert_eq!(links.open(&world, 1, 1), None);
    // The pair stays bound while it is closed.
    assert_eq!(links.bound(&world, 0, 11), Some((1, 1)));
    links.observe_mask(&chip(1), 0xfff, cause(7));
    assert_eq!(links.open(&world, 0, 11), Some((1, 1)));
}

#[test]
fn a_peer_on_a_chip_without_a_floor_leaves_the_portal_unbound() {
    // A world on chips 0 and 1; chip 2 is live and named by a peer record,
    // but has no floor here.
    let world = world_of(&[Some(0xfff), Some(0xfff)]);
    let mut links = all_live(3);
    links.observe_peer(&port(0, 4), &port(2, 1), cause(1));
    assert_eq!(
        links.peer(&chip(0), 4),
        Some((chip(2), 1)),
        "the table keeps it"
    );
    assert_eq!(links.bound(&world, 0, 4), None);
    assert_eq!(links.open(&world, 0, 4), None);

    // A far port with no portal on its floor leaves the near one unbound
    // too: chip 1's floor was born with port 6 dark, so it has no portal
    // for it, even after the port comes up.
    let world = world_of(&[Some(0xfff), Some(0xfff & !(1 << 6))]);
    links.observe_peer(&port(0, 5), &port(1, 6), cause(2));
    assert!(links.live(&chip(1), 6));
    assert_eq!(links.bound(&world, 0, 5), None);
    assert_eq!(links.open(&world, 0, 5), None);
    // And a near port with no portal has nothing to bind.
    let world = world_of(&[Some(0xfff & !(1 << 5)), Some(0xfff)]);
    assert_eq!(links.open(&world, 0, 5), None);
}

#[test]
fn peers_are_symmetric_and_a_new_pair_unlinks_the_old() {
    let mut links = Links::new();
    links.observe_peer(&port(0, 1), &port(1, 2), cause(1));
    // One record names both ends: a cable joins two ports.
    assert_eq!(links.peer(&chip(0), 1), Some((chip(1), 2)));
    assert_eq!(links.peer(&chip(1), 2), Some((chip(0), 1)));
    // Port 0/1 is recabled to chip 2: chip 1's old end no longer points at it.
    links.observe_peer(&port(0, 1), &port(2, 3), cause(2));
    assert_eq!(links.peer(&chip(0), 1), Some((chip(2), 3)));
    assert_eq!(links.peer(&chip(2), 3), Some((chip(0), 1)));
    assert_eq!(links.peer(&chip(1), 2), None);
    // A record from the other side that recables chip 2's end.
    links.observe_peer(&port(2, 3), &port(1, 9), cause(3));
    assert_eq!(links.peer(&chip(0), 1), None);
    assert_eq!(links.peer(&chip(1), 9), Some((chip(2), 3)));
}

#[test]
fn records_that_name_no_port_are_ignored() {
    let mut links = Links::new();
    // Not Ethernet ports, ports past 63, and a port cabled to itself.
    links.observe_peer(&chip(0), &port(1, 1), cause(1));
    links.observe_peer(&port(0, 1), &EntityKey::model("m"), cause(1));
    links.observe_peer(&port(0, 64), &port(1, 1), cause(1));
    links.observe_peer(&port(0, 1), &port(1, 200), cause(1));
    links.observe_peer(&EntityKey::dram_channel(&chip(0), 1), &port(1, 1), cause(1));
    links.observe_peer(&port(0, 1), &port(0, 1), cause(1));
    assert_eq!(links.chips.len(), 0, "{links:?}");
    assert!(!links.live(&chip(0), 64));
    assert_eq!(links.peer(&chip(0), 1), None);
}

#[test]
fn the_link_table_is_bounded() {
    // A hostile writer: 100 chips, every port up to 80, peers and masks in
    // random order. The table keeps at most MAX_CHIPS chips and 64 ports
    // each, and every peer entry has its mirror.
    let mut rng = Pcg32::new(7, 1);
    let mut links = Links::new();
    for n in 0..60_000u64 {
        let a = rng.range(0, 100) as u8;
        let b = rng.range(0, 100) as u8;
        let pa = rng.range(0, 80) as u8;
        let pb = rng.range(0, 80) as u8;
        if rng.chance(1, 8) {
            links.observe_mask(&chip(a), u64::from(rng.next_u32()) << 32 | n, cause(n));
        } else {
            links.observe_peer(&port(a, pa), &port(b, pb), cause(n));
        }
        if n % 997 == 0 {
            check_bounds(&links);
        }
    }
    check_bounds(&links);
    // The full table: 64 chips. A further chip is refused, whole.
    assert_eq!(links.chips.len(), MAX_CHIPS);
    let outsider = EntityKey::chip("zzzz:outsider");
    let inside = links.chips.keys().next().cloned().unwrap();
    links.observe_mask(&outsider, 1, cause(0));
    links.observe_peer(
        &EntityKey::eth_port(&inside, 0),
        &EntityKey::eth_port(&outsider, 0),
        cause(0),
    );
    assert!(!links.chips.contains_key(&outsider));
    assert_ne!(links.peer(&inside, 0), Some((outsider, 0)));
    check_bounds(&links);
}

#[test]
fn every_port_up_to_63_fits_and_64_does_not() {
    // Chips 0 to 7 each cable every port 0 to 79 to the next chip's port
    // 63 - p (ports 64 and up wrap to large numbers and are refused). A
    // later chip's records recable its ports, so the last one's (chip 8's)
    // whole range stays paired with chip 7: 64 peers, the per-chip cap.
    let mut links = Links::new();
    for c in 0..8u8 {
        for p in 0..80u8 {
            links.observe_peer(
                &port(c, p),
                &port(c + 1, 63u8.wrapping_sub(p)),
                cause(p.into()),
            );
            check_bounds(&links);
        }
    }
    assert_eq!(links.chips[&chip(8)].peers.len(), MAX_PORTS as usize);
    assert_eq!(links.peer(&chip(7), 63), Some((chip(8), 0)));
    assert_eq!(links.peer(&chip(8), 63), Some((chip(7), 0)));
    assert_eq!(links.peer(&chip(7), 64), None);
    // Masks: bit 63 counts, a port of 64 never does.
    links.observe_mask(&chip(8), 1 << 63, cause(0));
    assert!(links.live(&chip(8), 63));
    assert!(!links.live(&chip(8), 64));
}

/// The table's caps and its mirror rule.
fn check_bounds(links: &Links) {
    assert!(links.chips.len() <= MAX_CHIPS);
    for (c, l) in &links.chips {
        assert!(l.peers.len() <= MAX_PORTS as usize);
        for (&p, peer) in &l.peers {
            assert!(p < MAX_PORTS && peer.port < MAX_PORTS);
            assert_eq!(
                links.peer(&peer.chip, peer.port),
                Some((c.clone(), p)),
                "{c:?}/{p} has no mirror"
            );
        }
    }
}
