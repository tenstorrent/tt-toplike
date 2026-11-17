//! Tests for the maze generator.
//!
//! The property tests each run over 500 seeds. Seed `i` builds the floor for
//! one of several chip shapes (see [`spec_for`]) and one of several traffic
//! levels, so every shape and traffic level is covered many times.

use super::*;
use crate::topology::Arch;
use std::collections::VecDeque;
use tt_toplike_bus::EntityKey;

/// Seeds per property test.
const SEEDS: u64 = 500;

fn chip() -> EntityKey {
    EntityKey::chip("0000:01:00.0")
}

/// A spec with every field given, for shapes `for_chip` does not produce.
fn custom(
    cols: u8,
    rows: u8,
    dead_cols: Vec<u8>,
    dead_rows: Vec<u8>,
    vaults: (u8, u8),
    portals: u8,
) -> FloorSpec {
    FloorSpec {
        chip: chip(),
        arch: Arch::Generic,
        cols,
        rows,
        dead_cols,
        dead_rows,
        west_vaults: vaults.0,
        east_vaults: vaults.1,
        // Ports 0 to `portals - 1`: these shapes test the maze, which
        // places one portal per entry whatever the port numbers are.
        portal_ports: (0..portals).collect(),
    }
}

/// The chip shape used for seed `i`. It cycles through a plain Blackhole die,
/// Blackhole dies with harvested columns taken from the seed's bits, the
/// generic floor, the smallest possible grid, the largest possible grid, and
/// a spec with more vaults and portals than an edge has cells.
fn spec_for(i: u64) -> FloorSpec {
    match i % 6 {
        0 => FloorSpec::for_chip(&chip(), None, None, None, Arch::Blackhole),
        1 => {
            // Mix the seed so neighbouring seeds harvest different columns.
            let mask = i.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 20;
            FloorSpec::for_chip(&chip(), None, Some(mask), Some(mask >> 14), Arch::Blackhole)
        }
        2 => FloorSpec::generic(&chip()),
        3 => custom(1, 1, vec![], vec![], (1, 1), 1),
        4 => custom(120, 60, vec![3, 40, 119], vec![0, 59], (8, 8), 12),
        _ => custom(4, 3, vec![1], vec![2], (40, 40), 200),
    }
}

/// The traffic level used for seed `i`: idle, light, busy and saturated.
fn traffic_for(i: u64) -> f32 {
    [0.0, 0.3, 0.7, 1.0][(i % 4) as usize]
}

/// The floor for seed `i`.
fn floor_for(i: u64) -> Floor {
    generate(&spec_for(i), i, traffic_for(i))
}

/// Whether the hero can stand on a cell. This is written out here so the
/// reachability tests do not trust the generator's own rule: only `Wall`
/// blocks. `Ruin` is a collapsed wall and can be crossed.
fn passable(cell: Cell) -> bool {
    !matches!(cell, Cell::Wall)
}

/// Breadth-first search from `from` over passable cells (four neighbours).
/// Returns the set of reached cells as a `w * h` flag vector.
fn reach(floor: &Floor, from: (u8, u8)) -> Vec<bool> {
    let (w, h) = (floor.w as usize, floor.h as usize);
    let mut seen = vec![false; w * h];
    let start = from.1 as usize * w + from.0 as usize;
    if start >= seen.len() || !passable(floor.cells[start]) {
        return seen;
    }
    let mut queue = VecDeque::new();
    seen[start] = true;
    queue.push_back((from.0 as usize, from.1 as usize));
    while let Some((x, y)) = queue.pop_front() {
        let mut next = Vec::with_capacity(4);
        if x > 0 {
            next.push((x - 1, y));
        }
        if x + 1 < w {
            next.push((x + 1, y));
        }
        if y > 0 {
            next.push((x, y - 1));
        }
        if y + 1 < h {
            next.push((x, y + 1));
        }
        for (nx, ny) in next {
            let k = ny * w + nx;
            if !seen[k] && passable(floor.cells[k]) {
                seen[k] = true;
                queue.push_back((nx, ny));
            }
        }
    }
    seen
}

fn at(floor: &Floor, x: u8, y: u8) -> usize {
    y as usize * floor.w as usize + x as usize
}

#[test]
fn every_room_is_reachable_from_the_entrance() {
    for i in 0..SEEDS {
        let floor = floor_for(i);
        assert!(!floor.rooms.is_empty(), "seed {i}: no rooms");
        let seen = reach(&floor, floor.entrance);
        for (r, room) in floor.rooms.iter().enumerate() {
            // Every cell of the room, so a room split by a wall also fails.
            for y in room.y..room.y + room.h {
                for x in room.x..room.x + room.w {
                    assert!(
                        seen[at(&floor, x, y)],
                        "seed {i}: room {r} cell ({x}, {y}) unreachable"
                    );
                }
            }
        }
    }
}

#[test]
fn the_exit_and_every_vault_are_reachable() {
    for i in 0..SEEDS {
        let floor = floor_for(i);
        let spec = spec_for(i);
        let seen = reach(&floor, floor.entrance);
        assert!(
            seen[at(&floor, floor.exit.0, floor.exit.1)],
            "seed {i}: exit unreachable"
        );
        // The entrance is the gate.
        assert_eq!(
            floor.cells[at(&floor, floor.entrance.0, floor.entrance.1)],
            Cell::Gate,
            "seed {i}"
        );
        let mut vaults = 0;
        let mut portals = 0;
        for y in 0..floor.h {
            for x in 0..floor.w {
                let cell = floor.cells[at(&floor, x, y)];
                if matches!(cell, Cell::Vault | Cell::Portal | Cell::Gate) {
                    assert!(
                        seen[at(&floor, x, y)],
                        "seed {i}: {cell:?} at ({x}, {y}) unreachable"
                    );
                }
                if cell == Cell::Vault {
                    // Vaults sit on the west and east edges only.
                    assert!(
                        x == 0 || x == floor.w - 1,
                        "seed {i}: vault off the side edges"
                    );
                    vaults += 1;
                }
                if cell == Cell::Portal {
                    portals += 1;
                }
            }
        }
        // One vault per GDDR channel and one portal per live link, unless the
        // edge has fewer cells than that (spec shape 5 asks for too many).
        if i % 6 != 5 {
            assert_eq!(
                vaults,
                spec.west_vaults as usize + spec.east_vaults as usize,
                "seed {i}"
            );
            assert_eq!(portals, spec.portal_ports.len(), "seed {i}");
        } else {
            assert!(vaults > 0 && portals > 0, "seed {i}");
        }
    }
}

#[test]
fn no_floor_exceeds_the_size_bounds() {
    for i in 0..SEEDS {
        let floor = floor_for(i);
        assert!((12..=120).contains(&floor.w), "seed {i}: width {}", floor.w);
        assert!((8..=60).contains(&floor.h), "seed {i}: height {}", floor.h);
        assert_eq!(
            floor.cells.len(),
            floor.w as usize * floor.h as usize,
            "seed {i}"
        );
        for room in &floor.rooms {
            assert!(room.w > 0 && room.h > 0, "seed {i}: empty room");
            // Rooms stay inside the outer wall.
            assert!(room.x >= 1 && room.y >= 1, "seed {i}: room on the edge");
            assert!(
                (room.x as usize + room.w as usize) < (floor.w as usize),
                "seed {i}: room too wide"
            );
            assert!(
                (room.y as usize + room.h as usize) < (floor.h as usize),
                "seed {i}: room too tall"
            );
        }
        assert!(
            floor.entrance.0 < floor.w && floor.entrance.1 < floor.h,
            "seed {i}"
        );
        assert!(floor.exit.0 < floor.w && floor.exit.1 < floor.h, "seed {i}");
    }
}

#[test]
fn a_fully_dead_floor_still_has_one_room_and_a_path() {
    for i in 0..SEEDS {
        // Every column and row harvested, on a Blackhole-sized die and on the
        // largest grid.
        let (cols, rows) = if i % 2 == 0 { (17, 12) } else { (120, 60) };
        let spec = custom(
            cols,
            rows,
            (0..cols).collect(),
            (0..rows).collect(),
            (4, 4),
            12,
        );
        let floor = generate(&spec, i, traffic_for(i));
        assert!(!floor.rooms.is_empty(), "seed {i}: no rooms");
        let seen = reach(&floor, floor.entrance);
        // The exit must be a real room cell away from the gate, so a floor
        // whose rooms are all cut off cannot pass by falling back to the
        // entrance.
        assert_ne!(
            floor.exit, floor.entrance,
            "seed {i}: exit fell back to the entrance"
        );
        assert!(
            floor
                .rooms
                .iter()
                .any(|r| r.contains(floor.exit.0, floor.exit.1)),
            "seed {i}: exit outside every room"
        );
        assert!(
            seen[at(&floor, floor.exit.0, floor.exit.1)],
            "seed {i}: no path to the exit"
        );
        for (r, room) in floor.rooms.iter().enumerate() {
            let (x, y) = room.center();
            assert!(seen[at(&floor, x, y)], "seed {i}: room {r} cut off");
        }
        // The dead footprint shows as ruins.
        assert!(floor.cells.contains(&Cell::Ruin), "seed {i}: no ruins");
    }
}

#[test]
fn the_same_seed_gives_the_same_floor() {
    for i in 0..SEEDS {
        assert_eq!(floor_for(i), floor_for(i), "seed {i}");
    }
}

#[test]
fn different_seeds_give_different_floors_most_of_the_time() {
    // Pairs share a spec and traffic level, so only the seed differs.
    let mut differ = 0;
    for i in 0..SEEDS {
        let spec = spec_for(i);
        let a = generate(&spec, i, 0.5);
        let b = generate(&spec, i + 1_000_003, 0.5);
        if a != b {
            differ += 1;
        }
    }
    assert!(
        differ * 100 >= SEEDS * 95,
        "only {differ} of {SEEDS} pairs differ"
    );
}

#[test]
fn dead_columns_turn_walls_into_ruins_only_inside_their_footprint() {
    // One dead column on the generic floor: ruins appear, and only within
    // that column's cells. A 16 x 10 grid scales by 3 (16 * 3 + 2 = 50 fits
    // 120), and tile 5 covers cells 1 + 5 * 3 = 16 up to 19, worked out by
    // hand so the test does not reuse the generator's own arithmetic.
    let spec = custom(16, 10, vec![5], vec![], (2, 2), 2);
    let floor = generate(&spec, 7, 0.0);
    assert_eq!((floor.w, floor.h), (50, 32));
    let (lo, hi) = (16u8, 19u8);
    let mut ruins = 0;
    for y in 0..floor.h {
        for x in 0..floor.w {
            if floor.cells[at(&floor, x, y)] == Cell::Ruin {
                assert!(
                    (lo..hi).contains(&x),
                    "ruin at ({x}, {y}) outside columns {lo}..{hi}"
                );
                ruins += 1;
            }
        }
    }
    assert!(ruins > 0);
}

#[test]
fn the_gate_is_on_the_south_edge_and_portals_on_the_north() {
    for i in 0..50 {
        let floor = floor_for(i);
        assert_eq!(
            floor.entrance.1,
            floor.h - 1,
            "seed {i}: gate off the south edge"
        );
        for x in 0..floor.w {
            for y in 1..floor.h {
                assert_ne!(
                    floor.cells[at(&floor, x, y)],
                    Cell::Portal,
                    "seed {i}: portal off the north edge"
                );
            }
        }
    }
}

/// The cells of 500 Blackhole floors, each with its own seed, Ethernet mask,
/// Tensix mask and traffic level, folded into one FNV-1a hash (cell codes in
/// row-major order, floor after floor).
fn blackhole_cells_hash() -> u64 {
    let mut h = crate::seed::fnv1a64(&[]);
    for i in 0..SEEDS {
        let mix = i.wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(17) ^ i;
        // Every few seeds the mask is missing, so the "no reading" rule is
        // covered too.
        let eth = (i % 7 != 3).then_some(mix);
        let tensix = (i % 5 != 2).then_some(mix >> 23);
        let spec = FloorSpec::for_chip(&chip(), None, tensix, eth, Arch::Blackhole);
        let floor = generate(&spec, i ^ 0x5eed, traffic_for(i));
        let codes: Vec<u8> = floor.cells.iter().map(|&c| c as u8).collect();
        h = crate::seed::fnv1a64_extend(h, &codes);
        h = crate::seed::fnv1a64_extend(h, &[floor.w, floor.h]);
    }
    h
}

#[test]
fn a_blackhole_floor_keeps_its_cells() {
    // Pinned on 0.17.0, when a Blackhole spec held only a portal count (the
    // popcount of the low 12 bits of the mask, or 12 without a reading).
    // Binding portals to ports (task B.3) must not move one Blackhole cell:
    // the maze draws the same `spread` positions for the same count.
    assert_eq!(
        blackhole_cells_hash(),
        0xe470_d8ba_cd05_fa25,
        "Blackhole floor cells moved"
    );
}

#[test]
fn portals_are_listed_west_to_east_with_their_ports() {
    for i in 0..SEEDS {
        let floor = floor_for(i);
        let spec = spec_for(i);
        // One entry per port, in the spec's order, unless the north edge
        // has fewer cells than ports (shape 5): then the lowest ports get
        // the cells there are.
        let inner = floor.w as usize - 2;
        let kept = spec.portal_ports.len().min(inner);
        assert_eq!(floor.portals.len(), kept, "seed {i}");
        let ports: Vec<u8> = floor.portals.iter().map(|&(p, _)| p).collect();
        assert_eq!(ports, spec.portal_ports[..kept], "seed {i}");
        // West to east: x strictly ascending, every one on the north edge,
        // inside the corners, on a portal cell.
        for pair in floor.portals.windows(2) {
            assert!(pair[0].1 .0 < pair[1].1 .0, "seed {i}: {pair:?}");
        }
        for &(port, (x, y)) in &floor.portals {
            assert_eq!(y, 0, "seed {i}: port {port} off the north edge");
            assert!(
                x >= 1 && x + 1 < floor.w,
                "seed {i}: port {port} in a corner"
            );
            assert_eq!(floor.cell(x, y), Cell::Portal, "seed {i}: port {port}");
        }
        // And no portal cell is missing from the list.
        let cells = floor.cells.iter().filter(|&&c| c == Cell::Portal).count();
        assert_eq!(cells, floor.portals.len(), "seed {i}");
    }
    // The k-th live port, counted from the lowest, gets the k-th portal from
    // the west, whatever the port numbers are.
    let spec = FloorSpec::for_chip(&chip(), None, None, Some(0b1000_0010_0100), Arch::Blackhole);
    let floor = generate(&spec, 7, 0.0);
    let ports: Vec<u8> = floor.portals.iter().map(|&(p, _)| p).collect();
    assert_eq!(ports, vec![2, 5, 11]);
}
