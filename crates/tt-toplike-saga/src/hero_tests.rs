//! Tests for the hero, the quest planner and the pathfinder.
//!
//! The pathfinding tests do not trust the code under test to say what
//! "shortest" or "reachable" means. They carry their own breadth-first search
//! ([`bfs`]) and their own walkability rule ([`passable`]), and compare the
//! hero and [`astar`] against those.

use super::*;
use crate::event::Biome;
use crate::maze::{generate, Cell, Floor, Rect};
use crate::quest::{plan, Hook, Quest, Verb, REST_BELOW};
use crate::rng::Pcg32;
use crate::stats::TrailBook;
use crate::topology::{Arch, FloorSpec};
use crate::world::World;
use std::collections::VecDeque;
use tt_toplike_bus::EntityKey;

/// Floors in the big property test.
const FLOORS: u64 = 500;

fn chip() -> EntityKey {
    EntityKey::chip("0000:01:00.0")
}

/// A cause for quests the tests build by hand.
fn cause() -> Cause {
    Cause {
        entity: chip(),
        signal: "toplike.heartbeat".to_string(),
        ordinal: 7,
    }
}

fn quest_to(goal: (u8, u8)) -> Quest {
    Quest {
        verb: Verb::Explore,
        target: None,
        reason: cause(),
        goal,
        target_label: None,
    }
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
        portals,
    }
}

/// The chip shape for floor `i`: a plain Blackhole die, a harvested one, the
/// generic floor, the smallest grid, the largest grid with dead strips, and
/// a tiny grid with more vaults and portals than its edges hold.
fn spec_for(i: u64) -> FloorSpec {
    match i % 6 {
        0 => FloorSpec::for_chip(&chip(), None, None, None, Arch::Blackhole),
        1 => {
            let mask = i.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 20;
            FloorSpec::for_chip(&chip(), None, Some(mask), Some(mask >> 14), Arch::Blackhole)
        }
        2 => FloorSpec::generic(&chip()),
        3 => custom(1, 1, vec![], vec![], (1, 1), 1),
        4 => custom(120, 60, vec![3, 40, 119], vec![0, 59], (8, 8), 12),
        _ => custom(4, 3, vec![1], vec![2], (40, 40), 200),
    }
}

/// The floor for index `i`, with one of four traffic levels.
fn floor_for(i: u64) -> Floor {
    let traffic = [0.0, 0.3, 0.7, 1.0][(i % 4) as usize];
    generate(&spec_for(i), i.wrapping_mul(31).wrapping_add(5), traffic)
}

/// The test's own walkability rule: only `Wall` blocks.
fn passable(cell: Cell) -> bool {
    !matches!(cell, Cell::Wall)
}

/// Walking distance from `from` to every cell (4-way moves), `None` where
/// unreachable. Written here so the A* checks are independent of the code
/// under test.
fn bfs(floor: &Floor, from: (u8, u8)) -> Vec<Option<u32>> {
    let (w, h) = (floor.w as usize, floor.h as usize);
    let mut dist = vec![None; w * h];
    dist[from.1 as usize * w + from.0 as usize] = Some(0);
    let mut queue = VecDeque::from([(from.0 as usize, from.1 as usize)]);
    while let Some((x, y)) = queue.pop_front() {
        let d = dist[y * w + x].unwrap();
        let mut next = Vec::new();
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
            if dist[k].is_none() && passable(floor.cells[k]) {
                dist[k] = Some(d + 1);
                queue.push_back((nx, ny));
            }
        }
    }
    dist
}

fn dist_at(dist: &[Option<u32>], floor: &Floor, p: (u8, u8)) -> Option<u32> {
    dist[p.1 as usize * floor.w as usize + p.0 as usize]
}

/// Whether `a` and `b` are one 4-way step apart.
fn adjacent(a: (u8, u8), b: (u8, u8)) -> bool {
    a.0.abs_diff(b.0) as u32 + a.1.abs_diff(b.1) as u32 == 1
}

/// Asserts that `path` walks from `from` to `to` over passable cells, one
/// 4-way step at a time, inside the grid.
fn assert_valid_path(floor: &Floor, path: &[(u8, u8)], from: (u8, u8), to: (u8, u8)) {
    assert_eq!(
        path.first(),
        Some(&from),
        "a path starts where it is asked to"
    );
    assert_eq!(path.last(), Some(&to), "a path ends at the goal");
    for pair in path.windows(2) {
        assert!(
            adjacent(pair[0], pair[1]),
            "{:?} to {:?} is not one step",
            pair[0],
            pair[1]
        );
    }
    for &(x, y) in path {
        assert!(x < floor.w && y < floor.h, "({x}, {y}) is off the grid");
        assert!(passable(floor.cell(x, y)), "({x}, {y}) is a wall");
    }
}

/// A floor drawn as text: `#` is a wall, anything else is floor. Rooms,
/// entrance and exit are placeholders; the pathfinder reads only cells.
fn floor_from(rows: &[&str]) -> Floor {
    let h = rows.len() as u8;
    let w = rows[0].len() as u8;
    let cells = rows
        .iter()
        .flat_map(|r| r.chars())
        .map(|c| if c == '#' { Cell::Wall } else { Cell::Floor })
        .collect();
    Floor {
        w,
        h,
        cells,
        rooms: vec![Rect {
            x: 1,
            y: 1,
            w: 1,
            h: 1,
        }],
        entrance: (1, 1),
        exit: (1, 1),
    }
}

fn set(floor: &mut Floor, p: (u8, u8), cell: Cell) {
    let k = p.1 as usize * floor.w as usize + p.0 as usize;
    floor.cells[k] = cell;
}

#[test]
fn the_path_is_shortest_and_repeatable() {
    // On generated floors: from the entrance to every room centre and the
    // exit, the path is valid, as short as breadth-first search says, and
    // the same on a second call.
    for i in 0..60 {
        let floor = floor_for(i);
        let dist = bfs(&floor, floor.entrance);
        let mut goals: Vec<(u8, u8)> = floor.rooms.iter().map(|r| r.center()).collect();
        goals.push(floor.exit);
        for goal in goals {
            let want = dist_at(&dist, &floor, goal).expect("maze rooms are reachable");
            let path = astar(&floor, floor.entrance, goal).expect("a reachable goal has a path");
            assert_valid_path(&floor, &path, floor.entrance, goal);
            assert_eq!(
                path.len() as u32 - 1,
                want,
                "floor {i}: path to {goal:?} is longer than the shortest"
            );
            assert_eq!(
                astar(&floor, floor.entrance, goal),
                Some(path),
                "repeatable"
            );
        }
    }

    // On an open room every monotone route is shortest, so the tie rule
    // alone picks the path. Expanding the lowest (f, y, x) first fills the
    // top row before the next, which gives this exact route.
    let open = floor_from(&["#####", "#...#", "#...#", "#...#", "#####"]);
    let path = astar(&open, (1, 1), (3, 3)).unwrap();
    assert_eq!(path, vec![(1, 1), (2, 1), (3, 1), (3, 2), (3, 3)]);

    // A path to where the hero already stands is that one cell.
    assert_eq!(astar(&open, (2, 2), (2, 2)), Some(vec![(2, 2)]));
}

#[test]
fn the_hero_reaches_every_reachable_goal_on_500_floors() {
    let mut picks = Pcg32::from_seed(0x5a6a);
    for i in 0..FLOORS {
        let floor = floor_for(i);
        let mut hero = Hero::new("Tensix".to_string(), 0, floor.entrance, 0);
        // Goals: the exit, every vault, and four random cells (walls and
        // cut-off cells included, which must give no path).
        let mut goals = vec![floor.exit];
        for y in 0..floor.h {
            for x in 0..floor.w {
                if floor.cell(x, y) == Cell::Vault {
                    goals.push((x, y));
                }
            }
        }
        for _ in 0..4 {
            let x = picks.range(0, floor.w as u32) as u8;
            let y = picks.range(0, floor.h as u32) as u8;
            goals.push((x, y));
        }

        for goal in goals {
            // Each goal starts from wherever the hero stopped.
            let start = hero.pos;
            let dist = bfs(&floor, start);
            let quest = quest_to(goal);
            match dist_at(&dist, &floor, goal) {
                Some(want) => {
                    let mut calls = 0;
                    loop {
                        let before = hero.pos;
                        let r = hero.step(&floor, &quest);
                        calls += 1;
                        if r.moved {
                            assert!(adjacent(before, hero.pos), "floor {i}: the hero jumped");
                        } else {
                            assert_eq!(before, hero.pos, "floor {i}: moved without saying so");
                        }
                        assert!(hero.pos.0 < floor.w && hero.pos.1 < floor.h, "off the grid");
                        assert!(passable(floor.cell(hero.pos.0, hero.pos.1)), "in a wall");
                        assert!(!r.no_path, "floor {i}: {goal:?} is reachable");
                        if r.arrived {
                            break;
                        }
                        assert!(
                            calls <= want,
                            "floor {i}: {goal:?} took more than {want} steps"
                        );
                    }
                    assert_eq!(hero.pos, goal);
                    // One call per step, plus one to see arrival at distance 0.
                    assert_eq!(calls, want.max(1), "floor {i}: steps to {goal:?}");
                }
                None => {
                    assert_eq!(astar(&floor, start, goal), None, "floor {i}: {goal:?}");
                    let r = hero.step(&floor, &quest);
                    assert!(r.no_path && !r.moved && !r.arrived, "floor {i}: {r:?}");
                    assert_eq!(hero.pos, start, "a hero with no path stays put");
                }
            }
        }
    }
}

#[test]
fn a_blocked_goal_gives_no_path_and_the_hero_replans() {
    // The goal (5, 2) is walled in on every side.
    let mut floor = floor_from(&[
        "#########",
        "#...###.#",
        "#...#.#.#",
        "#...###.#",
        "#########",
    ]);
    let goal = (5, 2);
    assert_eq!(astar(&floor, (1, 1), goal), None);
    // A wall, or a cell off the grid, is never a goal.
    assert_eq!(astar(&floor, (1, 1), (0, 0)), None);
    assert_eq!(astar(&floor, (1, 1), (200, 2)), None);

    let mut hero = Hero::new("Tensix".to_string(), 0, (1, 1), 0);
    let quest = quest_to(goal);

    // The first step searches, finds nothing and stays put.
    let r = hero.step(&floor, &quest);
    assert_eq!(
        r,
        StepResult {
            moved: false,
            arrived: false,
            replanned: true,
            no_path: true
        }
    );
    assert_eq!(hero.pos, (1, 1));
    // It does not search again on every step: the stuck detector retries
    // after STUCK_STEPS idle steps.
    for n in 2..=STUCK_STEPS {
        let r = hero.step(&floor, &quest);
        assert!(r.no_path && !r.moved, "step {n}: {r:?}");
        assert!(!r.replanned, "step {n} searched again too soon");
        assert_eq!(hero.pos, (1, 1));
    }
    let r = hero.step(&floor, &quest);
    assert!(
        r.replanned && r.no_path,
        "the retry after the idle steps: {r:?}"
    );

    // Open a way in. The next retry finds it and the hero walks there.
    set(&mut floor, (4, 2), Cell::Floor);
    let mut steps = 0;
    while !hero.step(&floor, &quest).arrived {
        steps += 1;
        assert!(steps < 100, "the hero never reached the opened goal");
    }
    assert_eq!(hero.pos, goal);

    // A new quest searches at once, with no wait.
    let r = hero.step(&floor, &quest_to((1, 3)));
    assert!(r.replanned && r.moved, "{r:?}");
}

#[test]
fn the_stuck_detector_replans_after_twenty_idle_steps() {
    // A ring: two routes from the west room to (7, 2).
    let mut floor = floor_from(&[
        "#########",
        "#.......#",
        "#.#####.#",
        "#.......#",
        "#########",
    ]);
    let goal = (7, 2);
    let quest = quest_to(goal);
    let mut hero = Hero::new("Tensix".to_string(), 0, (1, 2), 0);
    assert_eq!(STUCK_STEPS, 20);

    // The tie rule takes the north route: (1, 1) first.
    let r = hero.step(&floor, &quest);
    assert!(r.moved && r.replanned);
    assert_eq!(hero.pos, (1, 1));

    // Something blocks the next cell. The hero waits, without searching.
    set(&mut floor, (2, 1), Cell::Wall);
    for n in 1..=STUCK_STEPS {
        let r = hero.step(&floor, &quest);
        assert!(!r.moved, "idle step {n} moved into a wall");
        assert!(!r.replanned, "idle step {n} replanned too soon");
        assert_eq!(hero.pos, (1, 1));
    }
    // After twenty idle steps it replans and takes the south route.
    let r = hero.step(&floor, &quest);
    assert!(r.replanned && r.moved, "{r:?}");
    assert_eq!(hero.pos, (1, 2));
    let mut steps = 0;
    while !hero.step(&floor, &quest).arrived {
        steps += 1;
        assert!(steps < 20);
    }
    assert_eq!(hero.pos, goal);
}

#[test]
fn vitality_rises_with_a_good_heartbeat_and_falls_on_a_stall() {
    let c = chip();
    let mut book = TrailBook::new();
    // With no readings there is no evidence of trouble.
    assert_eq!(vitality(&book, &c), 1.0);

    // A healthy chip: the heartbeat advances, clock and temperature steady.
    for _ in 0..50 {
        book.push(&c, HEARTBEAT_ALIVE, 1.0);
        book.push(&c, AI_CLOCK, 1000.0);
        push_temperature(&mut book, &c, 45.0);
    }
    let healthy = vitality(&book, &c);
    assert!(healthy > 0.99, "healthy chip: {healthy}");

    // The heartbeat stalls: vitality falls on every tick, towards zero.
    let mut last = healthy;
    for n in 0..48 {
        book.push(&c, HEARTBEAT_ALIVE, 0.0);
        let v = vitality(&book, &c);
        assert!(v < last, "stall tick {n}: {v} did not fall below {last}");
        last = v;
    }
    assert!(last < 0.1, "a long stall: {last}");

    // It comes back: vitality rises again on every tick.
    for n in 0..48 {
        book.push(&c, HEARTBEAT_ALIVE, 1.0);
        let v = vitality(&book, &c);
        assert!(v > last, "recovery tick {n}: {v} did not rise above {last}");
        last = v;
    }
    assert!(last > 0.9, "recovered: {last}");

    // A sharp temperature rise costs vitality even with a steady clock.
    let mut hot = book.clone();
    for _ in 0..10 {
        push_temperature(&mut hot, &c, 90.0);
    }
    let v = vitality(&hot, &c);
    assert!(v < last, "hot: {v} vs {last}");

    // Another chip's readings never count.
    assert_eq!(vitality(&book, &EntityKey::chip("0000:02:00.0")), 1.0);
}

/// Feeds one tick of readings for `chip`: a live heartbeat, a clock and a
/// temperature.
fn tick(book: &mut TrailBook, chip: &EntityKey, clock: f64, celsius: f64) {
    book.push(chip, HEARTBEAT_ALIVE, 1.0);
    book.push(chip, AI_CLOCK, clock);
    push_temperature(book, chip, celsius);
}

/// Explore and rest hooks, as every world offers them.
fn everyday_hooks() -> [Hook; 2] {
    [
        Hook {
            verb: Verb::Explore,
            target: None,
            reason: cause(),
        },
        Hook {
            verb: Verb::Rest,
            target: None,
            reason: cause(),
        },
    ]
}

/// A one-floor world for planning tests.
fn world_of(floor: Floor) -> World {
    World {
        id: 1,
        seed: 9,
        born_tick: 5,
        chips: vec![chip()],
        floors: vec![floor],
        biome: Biome::Dungeon,
        sage_chips: Vec::new(),
    }
}

#[test]
fn a_workload_ending_costs_no_vitality() {
    // A busy Blackhole: 1350 MHz at 70 degrees. The job ends. Over 30 ticks
    // the clock falls to its idle 800 MHz and the chip cools to 45 degrees,
    // then it idles for 2,000 ticks (longer than the 1,700 the clock's
    // trailing maximum needs to decay to within 90% of 800).
    let c = chip();
    let world = world_of(floor_for(0));
    let mut hero = Hero::new("Tensix".to_string(), 0, world.floors[0].entrance, 0);
    let mut book = TrailBook::new();
    for _ in 0..200 {
        tick(&mut book, &c, 1350.0, 70.0);
    }
    let mut lowest = 1.0f32;
    for i in 1..=2030u32 {
        let k = i.min(30) as f64 / 30.0;
        tick(&mut book, &c, 1350.0 - 550.0 * k, 70.0 - 25.0 * k);
        let v = vitality(&book, &c);
        lowest = lowest.min(v);
        assert!(
            v >= REST_BELOW,
            "tick {i}: an idle chip made the hero weak ({v})"
        );
        if i % 100 == 0 {
            hero.vitality = v;
            let q = plan(
                &world,
                &hero,
                &everyday_hooks(),
                &mut Pcg32::from_seed(i as u64),
            );
            assert_eq!(q.verb, Verb::Explore, "tick {i}: the hero rested at {v}");
        }
    }
    // Idling and cooling cost nothing at all.
    assert!(lowest > 0.999, "lowest vitality after the job: {lowest}");
}

#[test]
fn a_throttle_lowers_vitality_below_the_rest_line() {
    // A warm, steady chip: 1350 MHz at 60 degrees. Then the derived throttle
    // of spec 3.8: over 30 ticks the temperature climbs to 90 degrees while
    // the clock drops to 900 MHz, under 90% of its trailing maximum.
    let c = chip();
    let world = world_of(floor_for(0));
    let mut hero = Hero::new("Tensix".to_string(), 0, world.floors[0].entrance, 0);
    let mut book = TrailBook::new();
    for _ in 0..200 {
        tick(&mut book, &c, 1350.0, 60.0);
    }
    assert!(vitality(&book, &c) > 0.999);
    for i in 1..=30 {
        tick(&mut book, &c, 900.0, 60.0 + i as f64);
    }
    let v = vitality(&book, &c);
    assert!(v < REST_BELOW, "a throttle left vitality at {v}");
    hero.vitality = v;
    let q = plan(&world, &hero, &everyday_hooks(), &mut Pcg32::from_seed(1));
    assert_eq!(q.verb, Verb::Rest);

    // Held at 90 degrees, the rise passes: within about 50 ticks of the
    // temperature settling the hero is well again, though the clock stays
    // low and the chip stays hot.
    for _ in 0..80 {
        tick(&mut book, &c, 900.0, 90.0);
    }
    let v = vitality(&book, &c);
    assert!(v > 0.999, "a chip settled hot is healthy: {v}");
}

#[test]
fn plan_visits_the_sage_then_explores_or_rests() {
    let floor = floor_for(0);
    let world = World {
        id: 1,
        seed: 9,
        born_tick: 5,
        chips: vec![chip()],
        floors: vec![floor.clone()],
        biome: Biome::Dungeon,
        sage_chips: Vec::new(),
    };
    let sage = Hook {
        verb: Verb::VisitSage,
        target: Some(EntityKey::model("llama")),
        reason: cause(),
    };
    let explore = Hook {
        verb: Verb::Explore,
        target: None,
        reason: cause(),
    };
    let rest = Hook {
        verb: Verb::Rest,
        target: None,
        reason: cause(),
    };
    let hooks = [explore.clone(), rest.clone(), sage.clone()];
    let mut hero = Hero::new("Tensix".to_string(), 0, floor.entrance, 5);
    let mut rng = Pcg32::from_seed(1);

    // A ready model's sage comes first. The sage stands at the exit.
    let q = plan(&world, &hero, &hooks, &mut rng);
    assert_eq!(q.verb, Verb::VisitSage);
    assert_eq!(q.target, sage.target);
    assert_eq!(q.reason, sage.reason);
    assert_eq!(q.goal, floor.exit);

    // Once the hero is with the sage it moves on to explore a room.
    hero.pos = floor.exit;
    let q = plan(&world, &hero, &hooks, &mut rng);
    assert_eq!(q.verb, Verb::Explore);
    assert!(
        floor.rooms.iter().any(|r| r.center() == q.goal),
        "{:?}",
        q.goal
    );
    assert_ne!(q.goal, hero.pos);

    // A weak hero rests in the nearest room.
    hero.vitality = 0.2;
    let q = plan(&world, &hero, &hooks, &mut rng);
    assert_eq!(q.verb, Verb::Rest);
    assert!(floor.rooms.iter().any(|r| r.center() == q.goal));

    // The same inputs give the same quest.
    let a = plan(
        &world,
        &hero,
        std::slice::from_ref(&explore),
        &mut Pcg32::from_seed(3),
    );
    let b = plan(&world, &hero, &[explore], &mut Pcg32::from_seed(3));
    assert_eq!(a, b);

    // With no hooks at all the hero rests where it stands.
    let q = plan(&world, &hero, &[], &mut rng);
    assert_eq!(q.verb, Verb::Rest);
    assert_eq!(q.goal, hero.pos);
}
