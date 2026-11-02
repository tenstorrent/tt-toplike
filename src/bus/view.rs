//! What a view reads the bus through: [`BusView`] (a local host or a remote
//! world) and [`ReadCache`], which hands out a [`ReadView`] once per frame
//! without copying the world each time.
//!
//! # The cache
//!
//! A view calls [`BusView::read`] once per frame. The world changes only
//! when a record lands, which is at the data rate (10 Hz by default) or
//! slower, while frames come at up to 60 Hz. So the cache keeps one copy of
//! the world in an `Arc`, keyed by the sequence number it is current to:
//!
//! * Local: the key is [`tt_toplike_bus::Bus::head`] (one uncontended
//!   lock). When it moved, the world is copied once under the bus's
//!   publish lock ([`BusHost::with_world`]), which also gives the sequence
//!   number the copy is current to, so the key and the copy always agree.
//! * Attached: the key is the remote view's `(seq, instance)`
//!   ([`tt_toplike_bus::RemoteWorld::seq`]; the instance changes when the
//!   server restarts, possibly at the same `seq`). When it moved,
//!   [`tt_toplike_bus::RemoteWorld::latest`] copies the remote summary.
//!
//! When the key is unchanged, `read` costs one lock to read the key, one
//! lock on the slot, an `Arc` clone and a clock read. The clock is read on
//! every call (it is not part of the cached copy), so reading ages keep
//! growing while the bus is quiet.

use crate::bus::host::BusHost;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use tt_toplike_bus::{EntityKey, Event, HeaderKind, LocalView, ModelView, Reading, WorldRead};

/// Where the TUI's bus lives.
pub enum BusView {
    /// This process hosts the bus. Boxed: a host is large next to a
    /// remote world's handle.
    Local(Box<BusHost>),
    /// Another process (a `--headless` host) hosts it; this reads its socket.
    #[cfg(unix)]
    Attached(tt_toplike_bus::RemoteWorld),
}

impl BusView {
    /// The short label a view shows: `bus: local`, `bus: replay` (a replay
    /// bus, local or attached), or `bus: <socket path>` when attached.
    pub fn label(&self) -> String {
        match self {
            BusView::Local(h) => match h.bus().header().kind {
                HeaderKind::Replay => "bus: replay".to_string(),
                HeaderKind::Live => "bus: local".to_string(),
            },
            #[cfg(unix)]
            BusView::Attached(r) => match r.header_kind() {
                HeaderKind::Replay => "bus: replay".to_string(),
                HeaderKind::Live => format!("bus: {}", r.path().display()),
            },
        }
    }

    /// The world as a view reads it, from `cache`. See the module doc.
    pub fn read(&self, cache: &ReadCache) -> ReadView {
        match self {
            BusView::Local(h) => {
                let head = h.bus().head();
                let data = cache.get_or_rebuild((head, 0), || {
                    let kind = h.bus().header().kind;
                    let (seq, view) = h.with_world(|w, seq| {
                        (
                            seq,
                            LocalView {
                                seq,
                                world: w.clone(),
                                now_mono_ns: 0,
                                header_kind: kind,
                            },
                        )
                    });
                    ((seq, 0), Arc::new(view) as Arc<dyn WorldRead>)
                });
                ReadView {
                    data,
                    now_mono_ns: h.bus().now_mono_ns(),
                }
            }
            #[cfg(unix)]
            BusView::Attached(r) => {
                let key = (r.seq(), r.instance());
                let data = cache.get_or_rebuild(key, || {
                    let v = r.latest();
                    ((v.seq, v.instance), Arc::new(v) as Arc<dyn WorldRead>)
                });
                ReadView {
                    data,
                    now_mono_ns: r.now_mono_ns(),
                }
            }
        }
    }
}

/// The cached copy and the key it was built at.
struct Slot {
    key: (u64, u64),
    data: Arc<dyn WorldRead>,
}

/// One cached copy of the world. See the module doc.
#[derive(Default)]
pub struct ReadCache {
    slot: Mutex<Option<Slot>>,
    rebuilds: AtomicU64,
}

impl ReadCache {
    pub fn new() -> ReadCache {
        ReadCache::default()
    }

    /// The cached copy when its key is `key`; otherwise build a new one.
    /// `build` returns the copy and the key it is really current to (a
    /// record can land between reading `key` and building).
    fn get_or_rebuild(
        &self,
        key: (u64, u64),
        build: impl FnOnce() -> ((u64, u64), Arc<dyn WorldRead>),
    ) -> Arc<dyn WorldRead> {
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(s) = slot.as_ref() {
            if s.key == key {
                return s.data.clone();
            }
        }
        let (key, data) = build();
        self.rebuilds.fetch_add(1, Ordering::Relaxed);
        *slot = Some(Slot {
            key,
            data: data.clone(),
        });
        data
    }

    /// How many times the copy was rebuilt. For tests and the cost report.
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds.load(Ordering::Relaxed)
    }
}

/// What [`BusView::read`] returns: the cached world and the bus clock read
/// at the call. Cheap to clone.
#[derive(Clone)]
pub struct ReadView {
    data: Arc<dyn WorldRead>,
    now_mono_ns: u64,
}

impl WorldRead for ReadView {
    fn seq(&self) -> u64 {
        self.data.seq()
    }
    fn models(&self) -> Vec<ModelView> {
        self.data.models()
    }
    fn model_on_chip(&self, chip: &EntityKey) -> Option<ModelView> {
        self.data.model_on_chip(chip)
    }
    fn reading(&self, entity: &EntityKey, signal: &str) -> Option<Reading> {
        self.data.reading(entity, signal)
    }
    fn age_ms(&self, r: &Reading, now_mono_ns: u64) -> u64 {
        self.data.age_ms(r, now_mono_ns)
    }
    fn events_since(&self, seq: u64) -> Vec<(u64, Event)> {
        self.data.events_since(seq)
    }
    fn reset_seq(&self, chip: &EntityKey) -> Option<u64> {
        self.data.reset_seq(chip)
    }
    fn header_kind(&self) -> HeaderKind {
        self.data.header_kind()
    }
    /// The bus clock read when this view was handed out, so ages grow
    /// between records.
    fn now_mono_ns(&self) -> u64 {
        self.now_mono_ns
    }
}
