//! The link table: which Ethernet port is cabled to which, and which ports
//! are live (the party addendum, section 8.2).
//!
//! A floor's portals each belong to one Ethernet port
//! ([`crate::maze::Floor::portals`]). [`Links`] holds what the bus has said
//! about those ports, so the saga can tell which portals lead somewhere:
//!
//! * **Masks.** For each chip, its latest live-link mask
//!   (`toplike.eth.live_mask`, or the low 16 bits a `LinkChanged` carries),
//!   and the cause of the record that last changed it. Bit `n` set means
//!   port `n` is live. A chip whose mask was never seen counts as not live.
//! * **Peers.** For each port, its peer from the latest `LinkPeer` that
//!   names it (the record itself is not kept). A cable joins two ports, so one record sets both ends: port
//!   `a` to `b` and `b` to `a`. A record that recables a port first removes
//!   the old far end's entry when it still points back, so every entry has
//!   its mirror and the table never holds a half-link.
//!
//! # Bound and open
//!
//! For a world, a portal on floor `f` with port `p` is **bound** when port
//! `p` of floor `f`'s chip has a peer, the peer's chip has a floor in that
//! world, and the peer's port has a portal on that floor. It is **open**
//! while it is bound and both ends are live. So a portal is open only when
//! the mask bit says the port is up on both chips, whatever the peer record
//! says: a mask change with no new peer record (the party generator's chip
//! 0 at tick 600) closes the portal, and the peer record stays for when the
//! bit comes back.
//!
//! The peer's port must have a portal of its own because a hero who crosses
//! arrives on that portal's cell (the addendum, section 8.4). A far port
//! that was dark at the world's birth has no portal, so the link behind it
//! stays unbound for the world's life, as a link that comes up during a
//! world does (section 8.2: floors never change during a world).
//!
//! # Bounds
//!
//! The table keeps at most [`MAX_CHIPS`] chips and [`MAX_PORTS`] (64) ports
//! on each, so at most 4,096 peer entries. A chip enters the table the first
//! time a mask or peer record names it, while there is room. A record that
//! needs a chip the table has no room for is ignored whole, so a link is
//! never kept by one end only. The table keeps its own chips: a `LinkPeer`
//! from the bus source can name the next chip before that chip's first
//! reading (the source writes each chip's peers right after that chip's own
//! readings), so the saga's set of kept chips would drop it. A peer record
//! that names a chip the table cannot keep is ignored, and a peer chip that
//! has no floor in the world leaves the portal unbound. A writer that fills
//! the table with made-up chips first leaves the real ones without links;
//! memory stays bounded either way.
//!
//! Records that name no Ethernet port, a port number of 64 or more, a port
//! number spelled other than in plain decimal digits (`/+5`, `/05`), or
//! two ports of the same chip are ignored. A link always joins two chips,
//! so a floor is never bound to itself.
//!
//! # Determinism
//!
//! Every map is a `BTreeMap`, the table changes only in record order, and
//! it reads no clock. It is not hashed into the digest itself; the
//! crossing quest reads it (see "Crossing" in [`crate::saga`]), and the
//! digest sees the crossings it allows.

use std::collections::BTreeMap;

use tt_toplike_bus::{EntityKey, EntityKind};

use crate::hero::Cause;
use crate::world::{World, MAX_CHIPS};

/// Ports per chip the table keeps: one per bit of the 64-bit live mask.
pub const MAX_PORTS: u8 = 64;

/// One end of a link: the far chip and port. The record that set it is not
/// kept: its cause would hold an Ethernet port key per entry, up to 4,096
/// keys, and the saga's entity budget is 500 (spec 9).
#[derive(Debug, Clone, PartialEq)]
struct Peer {
    chip: EntityKey,
    port: u8,
}

/// What the table knows about one chip.
#[derive(Debug, Clone, Default, PartialEq)]
struct ChipLinks {
    /// The latest live-link mask, `None` until one is seen.
    mask: Option<u64>,
    /// The record that last changed `mask`.
    mask_cause: Option<Cause>,
    /// Each port's peer, at most [`MAX_PORTS`] entries.
    peers: BTreeMap<u8, Peer>,
}

/// The link table. See the module comment.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Links {
    /// At most [`MAX_CHIPS`] chips, in key order.
    chips: BTreeMap<EntityKey, ChipLinks>,
}

/// The chip and port number of an Ethernet port key
/// (`eth:chip:.../n`), or `None` for any other key and for a port of
/// [`MAX_PORTS`] or more.
///
/// The number must be spelled the way [`EntityKey::eth_port`] writes it:
/// ASCII digits only, with no leading zero unless the port is `0`. So
/// `/+5` and `/05` name no port. Rust's integer parse alone would read
/// both as port 5, and the table would then bind a port under a key that
/// no view prints.
fn split_port(key: &EntityKey) -> Option<(EntityKey, u8)> {
    if key.kind() != EntityKind::EthPort {
        return None;
    }
    let chip = key.chip_owner()?;
    let (_, n) = key.as_str().rsplit_once('/')?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) || (n.len() > 1 && n.starts_with('0'))
    {
        return None;
    }
    let port: u8 = n.parse().ok()?;
    (port < MAX_PORTS).then_some((chip, port))
}

impl Links {
    /// An empty table.
    pub fn new() -> Links {
        Links::default()
    }

    /// Whether the table holds every one of `chips` or has room for the
    /// ones it lacks.
    fn has_room_for(&self, chips: &[&EntityKey]) -> bool {
        let mut new: Vec<&EntityKey> = chips
            .iter()
            .copied()
            .filter(|c| !self.chips.contains_key(*c))
            .collect();
        new.sort();
        new.dedup();
        self.chips.len() + new.len() <= MAX_CHIPS
    }

    /// Records `chip`'s live-link mask. A changed mask (or the first one)
    /// also records `cause`. A chip the table has no room for is ignored.
    pub fn observe_mask(&mut self, chip: &EntityKey, mask: u64, cause: Cause) {
        if !self.has_room_for(&[chip]) {
            return;
        }
        let entry = self.chips.entry(chip.clone()).or_default();
        if entry.mask != Some(mask) {
            entry.mask = Some(mask);
            entry.mask_cause = Some(cause);
        }
    }

    /// Records that Ethernet port `port` is cabled to port `peer`, both
    /// ways. See the module comment for what is ignored. `cause` is the
    /// `LinkPeer` record; the table does not keep it (see [`Peer`]), and the
    /// argument is there so that every observation of the table takes its
    /// cause in the same way.
    pub fn observe_peer(&mut self, port: &EntityKey, peer: &EntityKey, cause: Cause) {
        let _ = cause;
        let (Some((a, pa)), Some((b, pb))) = (split_port(port), split_port(peer)) else {
            return;
        };
        // A cable between two ports of one chip (or a port and itself)
        // would bind a floor to itself; no hardware wires one, so the
        // record is ignored whole.
        if a == b || !self.has_room_for(&[&a, &b]) {
            return;
        }
        // Unlink each end's old far end, if it still points back.
        self.unlink(&a, pa);
        self.unlink(&b, pb);
        let near = Peer {
            chip: b.clone(),
            port: pb,
        };
        let far = Peer {
            chip: a.clone(),
            port: pa,
        };
        self.chips.entry(a).or_default().peers.insert(pa, near);
        self.chips.entry(b).or_default().peers.insert(pb, far);
    }

    /// Removes the link at `chip`/`port`, both ends, if there is one.
    fn unlink(&mut self, chip: &EntityKey, port: u8) {
        let Some(old) = self.chips.get_mut(chip).and_then(|l| l.peers.remove(&port)) else {
            return;
        };
        if let Some(far) = self.chips.get_mut(&old.chip) {
            if far
                .peers
                .get(&old.port)
                .is_some_and(|p| &p.chip == chip && p.port == port)
            {
                far.peers.remove(&old.port);
            }
        }
    }

    /// The chip and port that `chip`'s port `port` is cabled to, if a peer
    /// record said so.
    pub fn peer(&self, chip: &EntityKey, port: u8) -> Option<(EntityKey, u8)> {
        let p = self.chips.get(chip)?.peers.get(&port)?;
        Some((p.chip.clone(), p.port))
    }

    /// Whether `chip`'s port `port` is live: its latest mask has the bit
    /// set. A chip whose mask was never seen, and a port of [`MAX_PORTS`] or
    /// more, are not live.
    pub fn live(&self, chip: &EntityKey, port: u8) -> bool {
        port < MAX_PORTS
            && self
                .chips
                .get(chip)
                .and_then(|l| l.mask)
                .is_some_and(|m| m & (1u64 << port) != 0)
    }

    /// `chip`'s latest live-link mask, if one was seen.
    pub fn mask(&self, chip: &EntityKey) -> Option<u64> {
        self.chips.get(chip)?.mask
    }

    /// The record that last changed `chip`'s mask: the cause a crossing
    /// that fails on a link going down will carry (the addendum, section
    /// 8.5).
    pub fn mask_cause(&self, chip: &EntityKey) -> Option<&Cause> {
        self.chips.get(chip)?.mask_cause.as_ref()
    }

    /// The far floor and port of the portal on `floor` with port `port`,
    /// when it is bound in `world` (see "Bound and open" in the module
    /// comment), whether or not its ends are live.
    pub fn bound(&self, world: &World, floor: u8, port: u8) -> Option<(u8, u8)> {
        let has_portal = |f: usize, p: u8| {
            world
                .floors
                .get(f)
                .is_some_and(|fl| fl.portals.iter().any(|&(q, _)| q == p))
        };
        let chip = world.chips.get(floor as usize)?;
        if !has_portal(floor as usize, port) {
            return None;
        }
        let (far_chip, far_port) = self.peer(chip, port)?;
        // A world has at most MAX_FLOORS (32) chips, so the index fits a
        // byte.
        let far = world.chips.iter().position(|c| *c == far_chip)?;
        if !has_portal(far, far_port) {
            return None;
        }
        Some((far as u8, far_port))
    }

    /// The far floor and port of the portal on `floor` with port `port`,
    /// when it is bound in `world` and both of its ends are live.
    pub fn open(&self, world: &World, floor: u8, port: u8) -> Option<(u8, u8)> {
        let (far, far_port) = self.bound(world, floor, port)?;
        let near_chip = &world.chips[floor as usize];
        let far_chip = &world.chips[far as usize];
        (self.live(near_chip, port) && self.live(far_chip, far_port)).then_some((far, far_port))
    }
}

impl Links {
    /// Test only: every entity key the table holds (its chips, every
    /// peer's chip and every mask cause's entity), for the saga's entity
    /// budget check.
    #[cfg(test)]
    pub(crate) fn held_keys(&self) -> impl Iterator<Item = &EntityKey> {
        self.chips.iter().flat_map(|(c, l)| {
            std::iter::once(c)
                .chain(l.peers.values().map(|p| &p.chip))
                .chain(l.mask_cause.iter().map(|m| &m.entity))
        })
    }
}

#[cfg(test)]
#[path = "links_tests.rs"]
mod links_tests;
