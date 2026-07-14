// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Hivemindsweeper: opt-in passive activity sniffer. See
//! docs/superpowers/specs/2026-07-14-hivemindsweeper-design.md.

pub mod classify;
pub mod event;

use std::collections::VecDeque;
pub use event::{EventKind, Severity, SniffEvent, Source};

/// Bounded event history + drop accounting. Collectors are added in later tasks.
pub struct Hivemind {
    events: VecDeque<SniffEvent>,
    dropped: u64,
}

impl Hivemind {
    pub const RING_CAP: usize = 4096;

    pub fn new() -> Self {
        Self {
            events: VecDeque::with_capacity(256),
            dropped: 0,
        }
    }

    /// Append an event, evicting the oldest (and counting it) at capacity.
    pub fn push_for_test(&mut self, ev: SniffEvent) {
        self.ingest(ev);
    }

    fn ingest(&mut self, ev: SniffEvent) {
        if self.events.len() >= Self::RING_CAP {
            self.events.pop_front();
            self.dropped += 1;
        }
        self.events.push_back(ev);
    }

    pub fn events(&self) -> &VecDeque<SniffEvent> {
        &self.events
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

impl Default for Hivemind {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn ev() -> SniffEvent {
        SniffEvent {
            ts: Instant::now(),
            source: Source::Host,
            device: None,
            severity: Severity::Info,
            kind: EventKind::Log,
            text: "x".into(),
            origin: "test".into(),
        }
    }

    #[test]
    fn ring_evicts_oldest_and_counts_drops() {
        let mut h = Hivemind::new();
        for _ in 0..(Hivemind::RING_CAP + 5) {
            h.push_for_test(ev());
        }
        assert_eq!(h.events().len(), Hivemind::RING_CAP);
        assert_eq!(h.dropped(), 5);
    }
}
