//! The maze: one chip's dungeon floor, built from its [`FloorSpec`] and a seed.
//!
//! The footprint follows the chip. The seed decides the rooms and corridors.
//!
//! # Layout
//!
//! * **Grid.** Each tile of the chip's grid becomes a `scale x scale` block of
//!   cells, with one ring of outer wall around the whole floor. The scale is
//!   3 when the result fits the 120 x 60 budget, else 2, else 1 (see
//!   [`scale_for`]). The size is then clamped to at least 12 x 8 and at most
//!   120 x 60 cells. A Blackhole die (17 x 12 tiles) gives 53 x 38 cells.
//! * **Rooms.** The inside of the outer wall is the Tensix chamber floor. A
//!   binary space partition cuts it into leaves and each leaf gets one room.
//!   Leaves never overlap and every room keeps one wall cell from its leaf's
//!   east and south sides, so rooms never touch.
//! * **Corridors.** A spanning tree joins every room: starting from room 0,
//!   the generator repeatedly joins the unjoined room closest (by Manhattan
//!   distance between centres) to any joined room, with an L-shaped corridor
//!   whose bend direction comes from the seed. A few extra corridors between
//!   random rooms then add loops. `traffic` (0 to 1) is the chance that a
//!   corridor is two cells wide. Where a corridor leaves or enters a room the
//!   cell outside the room becomes a `Door`.
//! * **Ruins.** Every wall cell inside the outer ring that lies in a dead
//!   column's or dead row's block becomes `Ruin`. Ruins are collapsed walls:
//!   they can be walked across. Room and corridor cells there stay as they
//!   are. Since ruins only ever replace walls, they can join areas but can
//!   never cut a room off.
//! * **Vaults.** `west_vaults` cells on the west edge (x = 0) and
//!   `east_vaults` on the east edge (x = w - 1). Each edge is split into equal
//!   segments, one per vault, and the seed picks a row within each segment.
//!   The vertical order of GDDR channels within a side is not documented, so
//!   no channel number is tied to a row. A count larger than the edge's
//!   inner length is cut to that length.
//! * **Gate.** The PCIe gate sits on the south edge (y = h - 1) at a column
//!   from the seed. Die 3D draws the PCIe connector below the die's last row.
//! * **Portals.** One per live Ethernet link, on the north edge (y = 0),
//!   spread the same way as vaults. Die 3D draws the Ethernet cages above the
//!   die's first row. A count larger than the edge's inner length is cut.
//! * Every vault, portal and the gate gets a narrow corridor from the cell
//!   just inside it to the nearest room centre.
//! * **Entrance and exit.** The entrance is the gate. The exit is the centre
//!   of the room farthest from the entrance by walking distance (breadth-first
//!   search over walkable cells), with ties going to the lowest `(y, x)`.
//!
//! # Determinism
//!
//! Every choice comes from one [`Pcg32`] seeded with `seed`, drawn in a fixed
//! order. The code uses integer arithmetic and `Vec` indices only. The one
//! floating-point step is the `traffic` comparison, which uses `*` and `<`.

use std::collections::VecDeque;

use crate::rng::Pcg32;
use crate::topology::{FloorSpec, MAX_COLS, MAX_ROWS};

/// Smallest floor width in cells.
pub const MIN_W: u8 = 12;
/// Smallest floor height in cells.
pub const MIN_H: u8 = 8;

/// Smallest leaf side the partition makes. A leaf of 5 holds a room of 3 or
/// 4 cells plus a wall cell that separates it from the next leaf's room.
const MIN_LEAF: u8 = 5;
/// Smallest room side.
const MIN_ROOM: u8 = 3;
/// A leaf wider than this is always cut further.
const MAX_LEAF_W: u8 = 18;
/// A leaf taller than this is always cut further.
const MAX_LEAF_H: u8 = 12;

/// One cell of a floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    /// Solid rock. The only cell that cannot be walked on.
    Wall,
    /// A room or corridor cell.
    Floor,
    /// A collapsed wall in a harvested column or row. Walkable.
    Ruin,
    /// A GDDR vault on the west or east edge.
    Vault,
    /// The PCIe gate on the south edge. The entrance.
    Gate,
    /// An Ethernet portal on the north edge.
    Portal,
    /// Where a corridor meets a room.
    Door,
}

impl Cell {
    /// Whether the hero can stand here. Everything except `Wall` is walkable.
    pub fn walkable(self) -> bool {
        !matches!(self, Cell::Wall)
    }
}

/// An axis-aligned rectangle of cells. `x` and `y` are the north-west corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u8,
    pub y: u8,
    pub w: u8,
    pub h: u8,
}

impl Rect {
    /// The centre cell, rounded towards the north-west.
    pub fn center(&self) -> (u8, u8) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }

    /// Whether `(x, y)` lies inside the rectangle.
    pub fn contains(&self, x: u8, y: u8) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// A generated floor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Floor {
    /// Width in cells, [`MIN_W`] to [`MAX_COLS`].
    pub w: u8,
    /// Height in cells, [`MIN_H`] to [`MAX_ROWS`].
    pub h: u8,
    /// Row-major cells: `cells[y * w + x]`.
    pub cells: Vec<Cell>,
    /// Every room, in the order the partition made them. Never empty.
    pub rooms: Vec<Rect>,
    /// The gate cell.
    pub entrance: (u8, u8),
    /// The centre of the room farthest from the entrance.
    pub exit: (u8, u8),
}

impl Floor {
    /// The cell at `(x, y)`, or `Wall` outside the floor.
    pub fn cell(&self, x: u8, y: u8) -> Cell {
        if x >= self.w || y >= self.h {
            return Cell::Wall;
        }
        self.cells[y as usize * self.w as usize + x as usize]
    }
}

/// Cells per tile side for a `cols x rows` tile grid: 3 if the scaled grid
/// plus its outer wall fits 120 x 60, else 2, else 1.
fn scale_for(cols: u8, rows: u8) -> u8 {
    for s in [3u16, 2] {
        if cols as u16 * s + 2 <= MAX_COLS as u16 && rows as u16 * s + 2 <= MAX_ROWS as u16 {
            return s as u8;
        }
    }
    1
}

/// The cells `lo..hi` that tile `t` covers along one axis of a floor whose
/// size on that axis is `size`, cut to the inside of the outer wall. The span
/// is empty when the tile falls beyond a clamped floor.
fn tile_span(t: u8, scale: u8, size: u8) -> (u8, u8) {
    let inner_end = size.saturating_sub(1) as u16;
    let lo = (1 + t as u16 * scale as u16).min(inner_end);
    let hi = (lo + scale as u16).min(inner_end);
    (lo as u8, hi as u8)
}

/// Picks `count` distinct positions in `lo..hi`, one in each of `count` equal
/// segments, cutting `count` to the number of positions available.
fn spread(rng: &mut Pcg32, count: u8, lo: u8, hi: u8) -> Vec<u8> {
    let len = hi.saturating_sub(lo) as u32;
    let n = (count as u32).min(len);
    (0..n)
        .map(|i| {
            let a = lo as u32 + i * len / n;
            let b = lo as u32 + (i + 1) * len / n;
            rng.range(a, b) as u8
        })
        .collect()
}

/// Working state while a floor is carved.
struct Carver {
    w: u8,
    h: u8,
    cells: Vec<Cell>,
    /// For each cell, the index of the room it belongs to, if any.
    room_of: Vec<Option<u16>>,
}

impl Carver {
    fn idx(&self, x: u8, y: u8) -> usize {
        y as usize * self.w as usize + x as usize
    }

    /// Whether `(x, y)` lies inside the outer wall.
    fn inner(&self, x: u8, y: u8) -> bool {
        x >= 1 && y >= 1 && x + 1 < self.w && y + 1 < self.h
    }

    /// Turns a wall inside the outer ring into floor. Every other cell
    /// (rooms, doors, edge features) is left as it is.
    fn open(&mut self, x: u8, y: u8) {
        if self.inner(x, y) {
            let k = self.idx(x, y);
            if self.cells[k] == Cell::Wall {
                self.cells[k] = Cell::Floor;
            }
        }
    }

    /// Marks a corridor cell outside every room as a door.
    fn door(&mut self, x: u8, y: u8) {
        let k = self.idx(x, y);
        if self.room_of[k].is_none() && self.cells[k] == Cell::Floor {
            self.cells[k] = Cell::Door;
        }
    }

    /// Carves an L-shaped corridor from `a` to `b`, horizontal leg first when
    /// `across_first`. A `wide` corridor also opens the cell south of each
    /// horizontal step and east of each vertical step.
    fn corridor(&mut self, a: (u8, u8), b: (u8, u8), across_first: bool, wide: bool) {
        // The path as a list of (x, y, horizontal leg?) steps.
        let mut path = Vec::new();
        let (mut x, mut y) = a;
        let step = |v: u8, to: u8| if v < to { v + 1 } else { v - 1 };
        path.push((x, y, across_first));
        for leg in 0..2 {
            let horizontal = (leg == 0) == across_first;
            if horizontal {
                while x != b.0 {
                    x = step(x, b.0);
                    path.push((x, y, true));
                }
            } else {
                while y != b.1 {
                    y = step(y, b.1);
                    path.push((x, y, false));
                }
            }
        }

        for &(x, y, horizontal) in &path {
            self.open(x, y);
            if wide {
                if horizontal {
                    self.open(x, y + 1);
                } else {
                    self.open(x + 1, y);
                }
            }
        }

        // Doors where the main lane crosses a room's boundary.
        for pair in path.windows(2) {
            let (px, py, _) = pair[0];
            let (cx, cy, _) = pair[1];
            let was_in = self.room_of[self.idx(px, py)].is_some();
            let now_in = self.room_of[self.idx(cx, cy)].is_some();
            if was_in && !now_in {
                self.door(cx, cy);
            } else if !was_in && now_in {
                self.door(px, py);
            }
        }
    }
}

/// Whether a corridor drawn now is wide. Always draws once, so the random
/// stream is the same for every traffic level.
fn draw_wide(rng: &mut Pcg32, traffic: f32) -> bool {
    let roll = rng.range(0, 1000);
    (roll as f32) < traffic * 1000.0
}

/// Cuts the inside of the outer wall into leaves by binary space partition.
/// Leaves come out in the order an explicit stack pops them.
fn partition(rng: &mut Pcg32, inner: Rect) -> Vec<Rect> {
    let mut leaves = Vec::new();
    let mut stack = vec![inner];
    while let Some(r) = stack.pop() {
        let can_x = r.w >= 2 * MIN_LEAF;
        let can_y = r.h >= 2 * MIN_LEAF;
        let must = r.w > MAX_LEAF_W || r.h > MAX_LEAF_H;
        // Big leaves are always cut. A smaller leaf that could still be cut
        // is cut one time in four, which mixes small rooms with large ones.
        let cut = (can_x || can_y) && (must || rng.chance(1, 4));
        if !cut {
            leaves.push(r);
            continue;
        }
        // Cut across the longer side more often.
        let vertical = if can_x && can_y {
            rng.chance(r.w as u32, r.w as u32 + r.h as u32)
        } else {
            can_x
        };
        if vertical {
            let at = rng.range(MIN_LEAF as u32, (r.w - MIN_LEAF + 1) as u32) as u8;
            stack.push(Rect {
                x: r.x,
                y: r.y,
                w: at,
                h: r.h,
            });
            stack.push(Rect {
                x: r.x + at,
                y: r.y,
                w: r.w - at,
                h: r.h,
            });
        } else {
            let at = rng.range(MIN_LEAF as u32, (r.h - MIN_LEAF + 1) as u32) as u8;
            stack.push(Rect {
                x: r.x,
                y: r.y,
                w: r.w,
                h: at,
            });
            stack.push(Rect {
                x: r.x,
                y: r.y + at,
                w: r.w,
                h: r.h - at,
            });
        }
    }
    leaves
}

/// A room inside `leaf`, at least [`MIN_ROOM`] on each side, leaving the
/// leaf's last column and row as wall. The leaf is at least [`MIN_LEAF`]
/// on each side, so the room always fits.
fn room_in(rng: &mut Pcg32, leaf: Rect) -> Rect {
    let max_w = leaf.w - 1;
    let max_h = leaf.h - 1;
    let w = rng.range(MIN_ROOM as u32, max_w as u32 + 1) as u8;
    let h = rng.range(MIN_ROOM as u32, max_h as u32 + 1) as u8;
    let x = leaf.x + rng.range(0, (max_w - w) as u32 + 1) as u8;
    let y = leaf.y + rng.range(0, (max_h - h) as u32 + 1) as u8;
    Rect { x, y, w, h }
}

fn manhattan(a: (u8, u8), b: (u8, u8)) -> u32 {
    a.0.abs_diff(b.0) as u32 + a.1.abs_diff(b.1) as u32
}

/// The index of the room whose centre is closest to `p`, lowest index on ties.
fn nearest_room(rooms: &[Rect], p: (u8, u8)) -> usize {
    let mut best = 0;
    for (i, r) in rooms.iter().enumerate() {
        if manhattan(r.center(), p) < manhattan(rooms[best].center(), p) {
            best = i;
        }
    }
    best
}

/// Walking distance from `from` to every cell, `u32::MAX` where unreachable.
fn distances(c: &Carver, from: (u8, u8)) -> Vec<u32> {
    let (w, h) = (c.w as usize, c.h as usize);
    let mut dist = vec![u32::MAX; w * h];
    let start = c.idx(from.0, from.1);
    dist[start] = 0;
    let mut queue = VecDeque::from([(from.0 as usize, from.1 as usize)]);
    while let Some((x, y)) = queue.pop_front() {
        let d = dist[y * w + x];
        // West, east, north, south, in a fixed order.
        let next = [
            (x.wrapping_sub(1), y),
            (x + 1, y),
            (x, y.wrapping_sub(1)),
            (x, y + 1),
        ];
        for (nx, ny) in next {
            if nx >= w || ny >= h {
                continue;
            }
            let k = ny * w + nx;
            if dist[k] == u32::MAX && c.cells[k].walkable() {
                dist[k] = d + 1;
                queue.push_back((nx, ny));
            }
        }
    }
    dist
}

/// Builds the floor for `spec` from `seed`. `traffic` (0 to 1) is the chance
/// that each corridor is two cells wide; values outside that range act as
/// the nearer end, and NaN keeps every corridor narrow. The same arguments
/// always give the same floor. The
/// function does not panic for any spec.
pub fn generate(spec: &FloorSpec, seed: u64, traffic: f32) -> Floor {
    let mut rng = Pcg32::from_seed(seed);

    // Size from the tile grid.
    let scale = scale_for(spec.cols, spec.rows);
    let w = (spec.cols as u16 * scale as u16 + 2).clamp(MIN_W as u16, MAX_COLS as u16) as u8;
    let h = (spec.rows as u16 * scale as u16 + 2).clamp(MIN_H as u16, MAX_ROWS as u16) as u8;
    let n = w as usize * h as usize;
    #[cfg(test)]
    crate::cost::cells(n as u64);
    let mut c = Carver {
        w,
        h,
        cells: vec![Cell::Wall; n],
        room_of: vec![None; n],
    };

    // Rooms, one per partition leaf.
    let inner = Rect {
        x: 1,
        y: 1,
        w: w - 2,
        h: h - 2,
    };
    let leaves = partition(&mut rng, inner);
    let rooms: Vec<Rect> = leaves.iter().map(|&leaf| room_in(&mut rng, leaf)).collect();
    for (i, r) in rooms.iter().enumerate() {
        for y in r.y..r.y + r.h {
            for x in r.x..r.x + r.w {
                let k = c.idx(x, y);
                c.cells[k] = Cell::Floor;
                // Leaves are at least 5 x 5 inside a 118 x 58 interior, so
                // at most 23 * 11 = 253 rooms exist and the index fits u16.
                c.room_of[k] = Some(i as u16);
            }
        }
    }

    // Spanning tree (Prim's algorithm on centre distances). `best[j]` is the
    // closest joined room to room j and the distance to it.
    let count = rooms.len();
    let mut joined = vec![false; count];
    let mut best: Vec<(u32, usize)> = vec![(u32::MAX, 0); count];
    joined[0] = true;
    for j in 1..count {
        best[j] = (manhattan(rooms[0].center(), rooms[j].center()), 0);
    }
    for _ in 1..count {
        // The unjoined room closest to the tree; lowest index on ties.
        let mut pick = usize::MAX;
        for j in 0..count {
            if !joined[j] && (pick == usize::MAX || best[j].0 < best[pick].0) {
                pick = j;
            }
        }
        let from = best[pick].1;
        let across = rng.chance(1, 2);
        let wide = draw_wide(&mut rng, traffic);
        c.corridor(rooms[from].center(), rooms[pick].center(), across, wide);
        joined[pick] = true;
        for j in 0..count {
            let d = manhattan(rooms[pick].center(), rooms[j].center());
            if !joined[j] && d < best[j].0 {
                best[j] = (d, pick);
            }
        }
    }

    // A few loops: up to a quarter as many extra corridors as rooms.
    if count >= 3 {
        let loops = rng.range(0, count as u32 / 4 + 1);
        for _ in 0..loops {
            let a = rng.range(0, count as u32) as usize;
            let b = rng.range(0, count as u32) as usize;
            let across = rng.chance(1, 2);
            let wide = draw_wide(&mut rng, traffic);
            if a != b {
                c.corridor(rooms[a].center(), rooms[b].center(), across, wide);
            }
        }
    }

    // Edge features. Each is a cell on the outer ring plus a narrow corridor
    // from the cell just inside it to the nearest room centre.
    let place = |c: &mut Carver, rng: &mut Pcg32, at: (u8, u8), inside: (u8, u8), cell: Cell| {
        let k = c.idx(at.0, at.1);
        c.cells[k] = cell;
        let target = rooms[nearest_room(&rooms, inside)].center();
        let across = rng.chance(1, 2);
        c.corridor(inside, target, across, false);
    };
    for y in spread(&mut rng, spec.west_vaults, 1, h - 1) {
        place(&mut c, &mut rng, (0, y), (1, y), Cell::Vault);
    }
    for y in spread(&mut rng, spec.east_vaults, 1, h - 1) {
        place(&mut c, &mut rng, (w - 1, y), (w - 2, y), Cell::Vault);
    }
    for x in spread(&mut rng, spec.portals, 1, w - 1) {
        place(&mut c, &mut rng, (x, 0), (x, 1), Cell::Portal);
    }
    let gate_x = rng.range(1, w as u32 - 1) as u8;
    let entrance = (gate_x, h - 1);
    place(&mut c, &mut rng, entrance, (gate_x, h - 2), Cell::Gate);

    // Ruins: walls in the blocks of dead columns and rows collapse.
    let mut dead_x = vec![false; w as usize];
    let mut dead_y = vec![false; h as usize];
    for &col in &spec.dead_cols {
        let (lo, hi) = tile_span(col, scale, w);
        for x in lo..hi {
            dead_x[x as usize] = true;
        }
    }
    for &row in &spec.dead_rows {
        let (lo, hi) = tile_span(row, scale, h);
        for y in lo..hi {
            dead_y[y as usize] = true;
        }
    }
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let k = c.idx(x, y);
            if (dead_x[x as usize] || dead_y[y as usize]) && c.cells[k] == Cell::Wall {
                c.cells[k] = Cell::Ruin;
            }
        }
    }

    // Exit: the farthest room centre by walking distance, ties to the lowest
    // (y, x). Every room is joined, so every centre is reachable; the entrance
    // stays the fallback all the same.
    let dist = distances(&c, entrance);
    let mut far: Option<(u32, u8, u8)> = None;
    for r in &rooms {
        let (x, y) = r.center();
        let d = dist[c.idx(x, y)];
        if d == u32::MAX {
            continue;
        }
        let better = match far {
            None => true,
            Some((fd, fy, fx)) => d > fd || (d == fd && (y, x) < (fy, fx)),
        };
        if better {
            far = Some((d, y, x));
        }
    }
    let exit = far.map_or(entrance, |(_, y, x)| (x, y));

    Floor {
        w,
        h,
        cells: c.cells,
        rooms,
        entrance,
        exit,
    }
}

#[cfg(test)]
#[path = "maze_tests.rs"]
mod maze_tests;
