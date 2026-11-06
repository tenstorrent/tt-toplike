//! Chip identity and the index-to-key map.
//!
//! A chip is identified by its PCI address (`Device.bus_id`), through
//! `EntityKey::chip`. The `/dev/tenstorrent/N` index is not stable: two chips
//! can swap indexes after a reset or a hotplug. Sources that only know an
//! index (processes, training, hivemind, `ServiceState.chips`) therefore look
//! the key up in this map when they write a record, and the map is rebuilt
//! from the backend's device list on every discovery pass.
//!
//! Rules:
//! * `refresh` replaces the whole map in one step. A reader sees the old map
//!   or the new one, never a mix.
//! * Devices that share a `bus_id` collapse to one key. Both indexes resolve
//!   to it, and [`ChipInfo::index`] holds the lowest.
//! * A device with an empty `bus_id` gets no key. A key is never made from
//!   the loop position. Such devices are counted in [`IdentityMap::unkeyed`].
//! * A payload for an index that does not resolve yet is held with
//!   [`IdentityMap::hold_unresolved`] for exactly one `refresh`. If the index
//!   resolves then, [`IdentityMap::resolve_pending`] returns the payload. If
//!   not, it is dropped and counted in [`IdentityMap::dropped`].
//! * Both lists (held and resolved) keep at most [`MAX_PENDING`] (1024)
//!   payloads. At the limit the oldest is dropped and counted in
//!   [`IdentityMap::dropped`].

use crate::models::device::{Architecture, Device};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use tt_toplike_bus::{EntityKey, Payload};

/// Most payloads kept in `held`, and separately in `ready`. When a list is
/// full the oldest entry is dropped and counted in `dropped()`.
pub const MAX_PENDING: usize = 1024;

/// What the map remembers about one chip, for later use by sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChipInfo {
    /// The device index the chip had at the last `refresh` (lowest, if the
    /// same `bus_id` was listed more than once).
    pub index: usize,
    pub board_type: String,
    pub architecture: Architecture,
    pub bus_id: String,
}

/// A held payload whose index resolved at a `refresh`.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingResolved {
    pub key: EntityKey,
    pub payload: Payload,
}

/// Push onto a list capped at [`MAX_PENDING`], dropping the oldest entry when
/// full. Returns how many entries were dropped (0 or 1).
fn push_bounded<T>(list: &mut VecDeque<T>, item: T) -> u64 {
    let mut dropped = 0;
    if list.len() >= MAX_PENDING {
        list.pop_front();
        dropped = 1;
    }
    list.push_back(item);
    dropped
}

#[derive(Default)]
struct State {
    by_index: HashMap<usize, EntityKey>,
    info: HashMap<EntityKey, ChipInfo>,
    /// Payloads waiting for their index to appear, held for one `refresh`.
    held: VecDeque<(usize, Payload)>,
    /// Payloads that resolved at the last `refresh`, waiting for pickup.
    ready: VecDeque<PendingResolved>,
}

/// Shared map from device index to chip key. Safe to share as
/// `Arc<IdentityMap>` across sources.
#[derive(Default)]
pub struct IdentityMap {
    state: RwLock<State>,
    dropped: AtomicU64,
    unkeyed: AtomicU64,
}

impl IdentityMap {
    pub fn new() -> Self {
        Self::default()
    }

    // A panic in another thread must not turn every later lookup into a
    // panic, and the state is always left consistent, so recover from poison.
    fn read(&self) -> RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(|e| e.into_inner())
    }
    fn write(&self) -> RwLockWriteGuard<'_, State> {
        self.state.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Rebuild the map from the backend's current device list.
    ///
    /// Also settles held payloads: those whose index now resolves move to the
    /// pickup list (see [`resolve_pending`](Self::resolve_pending)); the rest
    /// are dropped and counted.
    pub fn refresh(&self, devices: &[Device]) {
        // Build the new maps outside the lock, then swap them in together.
        let mut by_index = HashMap::new();
        let mut info: HashMap<EntityKey, ChipInfo> = HashMap::new();
        let mut unkeyed = 0u64;
        for d in devices {
            if d.bus_id.is_empty() {
                unkeyed += 1;
                continue;
            }
            let key = EntityKey::chip(&d.bus_id);
            by_index.insert(d.index, key.clone());
            let entry = ChipInfo {
                index: d.index,
                board_type: d.board_type.clone(),
                architecture: d.architecture,
                bus_id: d.bus_id.clone(),
            };
            match info.get_mut(&key) {
                // Same bus_id again: keep one entry, with the lowest index.
                Some(existing) if existing.index <= d.index => {}
                Some(existing) => *existing = entry,
                None => {
                    info.insert(key, entry);
                }
            }
        }
        if unkeyed > 0 {
            self.unkeyed.fetch_add(unkeyed, Ordering::Relaxed);
        }

        let mut st = self.write();
        let held = std::mem::take(&mut st.held);
        let mut dropped = 0u64;
        for (index, payload) in held {
            match by_index.get(&index) {
                Some(key) => {
                    dropped += push_bounded(
                        &mut st.ready,
                        PendingResolved {
                            key: key.clone(),
                            payload,
                        },
                    )
                }
                None => dropped += 1,
            }
        }
        st.by_index = by_index;
        st.info = info;
        drop(st);
        if dropped > 0 {
            self.dropped.fetch_add(dropped, Ordering::Relaxed);
        }
    }

    /// The chip key for a device index, if the last `refresh` listed one.
    pub fn key_for_index(&self, index: usize) -> Option<EntityKey> {
        self.read().by_index.get(&index).cloned()
    }

    /// What is known about a chip, if the last `refresh` listed it.
    pub fn info_for(&self, key: &EntityKey) -> Option<ChipInfo> {
        self.read().info.get(key).cloned()
    }

    /// Keep a payload whose index does not resolve yet, for one `refresh`.
    ///
    /// The index is checked again under the write lock. A source can look up
    /// an index, miss, lose a race to `refresh`, and only then call this; if
    /// the index resolves by now the payload goes straight to the pickup list.
    /// Both lists are capped at [`MAX_PENDING`]; the oldest entry is dropped
    /// and counted when one is full.
    pub fn hold_unresolved(&self, index: usize, payload: Payload) {
        let mut st = self.write();
        let dropped = match st.by_index.get(&index).cloned() {
            Some(key) => push_bounded(&mut st.ready, PendingResolved { key, payload }),
            None => push_bounded(&mut st.held, (index, payload)),
        };
        drop(st);
        if dropped > 0 {
            self.dropped.fetch_add(dropped, Ordering::Relaxed);
        }
    }

    /// Take the payloads that resolved at the last `refresh`.
    pub fn resolve_pending(&self) -> Vec<PendingResolved> {
        std::mem::take(&mut self.write().ready).into()
    }

    /// Held payloads dropped: an index that never resolved, or a list already
    /// at [`MAX_PENDING`] (`identity.dropped`).
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Devices seen without a `bus_id`, summed over all `refresh` calls
    /// (`identity.unkeyed`).
    pub fn unkeyed(&self) -> u64 {
        self.unkeyed.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use tt_toplike_bus::{Gap, Payload};

    fn dev(index: usize, board: &str, bus: &str) -> Device {
        Device::new(index, board.into(), bus.into(), String::new())
    }
    fn gap(s: &str) -> Payload {
        Payload::Gap(Gap { reason: s.into() })
    }
    const A: &str = "0000:01:00.0";
    const B: &str = "0000:02:00.0";

    #[test]
    fn swapped_indexes_resolve_to_the_right_chips() {
        let m = IdentityMap::new();
        m.refresh(&[dev(0, "n150", A), dev(1, "p150", B)]);
        assert_eq!(m.key_for_index(0), Some(EntityKey::chip(A)));
        m.refresh(&[dev(0, "p150", B), dev(1, "n150", A)]);
        assert_eq!(m.key_for_index(0), Some(EntityKey::chip(B)));
        assert_eq!(m.key_for_index(1), Some(EntityKey::chip(A)));
        let info = m.info_for(&EntityKey::chip(A)).unwrap();
        assert_eq!(info.index, 1);
        assert_eq!(info.board_type, "n150");
        assert_eq!(info.bus_id, A);
    }

    #[test]
    fn departed_chip_stops_resolving() {
        let m = IdentityMap::new();
        m.refresh(&[dev(0, "n150", A), dev(1, "n150", B)]);
        m.refresh(&[dev(0, "n150", A)]);
        assert_eq!(m.key_for_index(1), None);
        assert!(m.info_for(&EntityKey::chip(B)).is_none());
    }

    #[test]
    fn held_record_is_delivered_when_the_index_appears() {
        let m = IdentityMap::new();
        m.refresh(&[dev(0, "n150", A)]);
        m.hold_unresolved(1, gap("x"));
        assert!(m.resolve_pending().is_empty(), "nothing before a refresh");
        m.refresh(&[dev(0, "n150", A), dev(1, "n150", B)]);
        let got = m.resolve_pending();
        assert_eq!(
            got,
            vec![PendingResolved {
                key: EntityKey::chip(B),
                payload: gap("x")
            }]
        );
        assert_eq!(m.dropped(), 0);
        assert!(m.resolve_pending().is_empty(), "delivered once");
    }

    #[test]
    fn held_record_is_dropped_and_counted_when_the_index_never_appears() {
        let m = IdentityMap::new();
        m.refresh(&[dev(0, "n150", A)]);
        m.hold_unresolved(7, gap("x"));
        m.refresh(&[dev(0, "n150", A)]);
        assert!(m.resolve_pending().is_empty());
        assert_eq!(m.dropped(), 1);
        // Held for exactly one refresh: a later appearance does not revive it.
        m.refresh(&[dev(0, "n150", A), dev(7, "n150", B)]);
        assert!(m.resolve_pending().is_empty());
        assert_eq!(m.dropped(), 1);
    }

    #[test]
    fn same_bus_id_collapses_to_one_key() {
        let m = IdentityMap::new();
        m.refresh(&[dev(3, "n150", A), dev(1, "n150", A)]);
        assert_eq!(m.key_for_index(3), Some(EntityKey::chip(A)));
        assert_eq!(m.key_for_index(1), Some(EntityKey::chip(A)));
        assert_eq!(m.info_for(&EntityKey::chip(A)).unwrap().index, 1);
        assert_eq!(m.unkeyed(), 0);
    }

    #[test]
    fn empty_bus_id_gets_no_key_and_is_counted() {
        let m = IdentityMap::new();
        m.refresh(&[dev(0, "n150", ""), dev(1, "n150", A)]);
        assert_eq!(m.key_for_index(0), None);
        assert_eq!(m.key_for_index(1), Some(EntityKey::chip(A)));
        assert_eq!(m.unkeyed(), 1);
    }

    #[test]
    fn held_list_is_bounded_and_keeps_the_newest() {
        let m = IdentityMap::new();
        for i in 0..5000 {
            m.hold_unresolved(9, gap(&i.to_string()));
        }
        let st = m.read();
        assert!(st.held.len() <= MAX_PENDING);
        assert_eq!(m.dropped(), (5000 - MAX_PENDING) as u64);
        assert_eq!(
            st.held.front().unwrap().1,
            gap(&(5000 - MAX_PENDING).to_string())
        );
        assert_eq!(st.held.back().unwrap().1, gap("4999"));
    }

    #[test]
    fn ready_list_is_bounded_and_keeps_the_newest() {
        let m = IdentityMap::new();
        m.refresh(&[dev(0, "n150", A)]);
        // Each hold resolves at once, and nobody calls resolve_pending.
        for i in 0..5000 {
            m.hold_unresolved(0, gap(&i.to_string()));
        }
        assert_eq!(m.dropped(), (5000 - MAX_PENDING) as u64);
        let got = m.resolve_pending();
        assert_eq!(got.len(), MAX_PENDING);
        assert_eq!(got[0].payload, gap(&(5000 - MAX_PENDING).to_string()));
        assert_eq!(got.last().unwrap().payload, gap("4999"));
    }

    #[test]
    fn hold_after_the_index_resolved_goes_straight_to_ready() {
        let m = IdentityMap::new();
        m.refresh(&[dev(0, "n150", A)]);
        assert_eq!(m.key_for_index(1), None); // the source's lookup
        m.refresh(&[dev(0, "n150", A), dev(1, "n150", B)]); // refresh races in
        m.hold_unresolved(1, gap("late")); // the source's hold
        assert_eq!(
            m.resolve_pending(),
            vec![PendingResolved {
                key: EntityKey::chip(B),
                payload: gap("late")
            }]
        );
        assert_eq!(m.dropped(), 0);
    }

    #[test]
    fn concurrent_refresh_and_lookup_never_return_a_wrong_key() {
        let m = Arc::new(IdentityMap::new());
        let (ca, cb, cc) = ("0000:0a:00.0", "0000:0b:00.0", "0000:0c:00.0");
        let l1 = vec![dev(0, "n150", ca), dev(1, "n150", cb), dev(2, "n150", cc)];
        let l2 = vec![dev(0, "n150", cb), dev(1, "n150", ca), dev(2, "n150", cc)];
        m.refresh(&l1);
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let (m, stop) = (m.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut i = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    m.refresh(if i % 2 == 0 { &l2 } else { &l1 });
                    i += 1;
                }
            })
        };
        let reader = {
            let m = m.clone();
            std::thread::spawn(move || {
                for _ in 0..200_000 {
                    // Index 0 is always one of the swapping pair.
                    let k0 = m.key_for_index(0).expect("index 0 always listed");
                    assert!(k0 == EntityKey::chip(ca) || k0 == EntityKey::chip(cb));
                    // The info for that key names the same chip.
                    let info = m.info_for(&k0).expect("info for a listed key");
                    assert_eq!(EntityKey::chip(&info.bus_id), k0);
                    // Index 2 never moves and must never vanish mid-refresh.
                    assert_eq!(m.key_for_index(2), Some(EntityKey::chip(cc)));
                    // Index 9 is never listed.
                    assert_eq!(m.key_for_index(9), None);
                }
            })
        };
        reader.join().unwrap();
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
    }
}
