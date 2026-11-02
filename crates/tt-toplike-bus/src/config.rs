//! Bus settings and the clock the bus reads its timestamps from.
//!
//! [`BusConfig::default`] keeps the log in memory only. Set `dir` to also
//! write segment files to disk.
//!
//! The clock is a setting so tests can move time forward without sleeping.
//! Segment rotation and retention use the timestamps the clock gives, so a
//! [`ManualClock`] makes both deterministic.

use crate::record::HeaderKind;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Where the bus gets the two timestamps every record carries.
pub trait Clock: Send + Sync {
    /// Monotonic time in nanoseconds. It never goes backwards.
    fn mono_ns(&self) -> u64;
    /// Wall-clock time in milliseconds since the Unix epoch.
    fn wall_ms(&self) -> u64;
}

/// The real clock. Monotonic time counts from the first time any
/// `SystemClock` is read in this process.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn mono_ns(&self) -> u64 {
        static BASE: OnceLock<Instant> = OnceLock::new();
        let base = *BASE.get_or_init(Instant::now);
        u64::try_from(base.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    fn wall_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            // A wall clock set before 1970 reports 0.
            .unwrap_or(0)
    }
}

/// A clock that only moves when told to. Both timestamps advance together.
#[derive(Debug, Default)]
pub struct ManualClock {
    mono_ns: AtomicU64,
    wall_ms: AtomicU64,
}

impl ManualClock {
    /// A clock standing at `wall_ms` with monotonic time 0.
    pub fn new(wall_ms: u64) -> Arc<ManualClock> {
        Arc::new(ManualClock {
            mono_ns: AtomicU64::new(0),
            wall_ms: AtomicU64::new(wall_ms),
        })
    }

    /// Move both timestamps forward by `d`.
    pub fn advance(&self, d: Duration) {
        let ns = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        let ms = u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        self.mono_ns.fetch_add(ns, Ordering::SeqCst);
        self.wall_ms.fetch_add(ms, Ordering::SeqCst);
    }

    /// Move both timestamps forward by `secs` seconds.
    pub fn advance_secs(&self, secs: u64) {
        self.advance(Duration::from_secs(secs));
    }
}

impl Clock for ManualClock {
    fn mono_ns(&self) -> u64 {
        self.mono_ns.load(Ordering::SeqCst)
    }

    fn wall_ms(&self) -> u64 {
        self.wall_ms.load(Ordering::SeqCst)
    }
}

/// Settings for [`crate::Bus::open`].
#[derive(Clone)]
pub struct BusConfig {
    /// Directory for segment files. `None` keeps the log in memory only.
    pub dir: Option<PathBuf>,
    /// How many of the newest records stay in memory. At least one is kept.
    pub window_records: usize,
    /// A segment file is closed and compressed once it is this many seconds
    /// old (measured from its first record).
    pub segment_secs: u64,
    /// Closed segments that ended more than this many seconds ago are deleted.
    pub retain_secs: u64,
    /// While all segments together are larger than this many bytes, the
    /// oldest closed segment is deleted. The open segment is never deleted.
    pub retain_bytes: u64,
    /// How many records may wait for the disk writer. When the queue is full
    /// a record is still published and kept in memory, but its disk write is
    /// dropped and counted.
    pub writer_queue: usize,
    /// Source of timestamps. Tests use a [`ManualClock`].
    pub clock: Arc<dyn Clock>,
    /// Written into each segment header and returned by [`crate::Bus::header`].
    /// `Live` by default. [`crate::Replay::bus_config`] sets `Replay`.
    pub header_kind: HeaderKind,
}

impl Default for BusConfig {
    fn default() -> Self {
        BusConfig {
            dir: None,
            window_records: 65_536,
            // Ten minutes per segment.
            segment_secs: 10 * 60,
            // Keep 24 hours or 5 GB, whichever limit is reached first.
            retain_secs: 24 * 60 * 60,
            retain_bytes: 5 * 1000 * 1000 * 1000,
            writer_queue: 16_384,
            clock: Arc::new(SystemClock),
            header_kind: HeaderKind::Live,
        }
    }
}

impl fmt::Debug for BusConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BusConfig")
            .field("dir", &self.dir)
            .field("window_records", &self.window_records)
            .field("segment_secs", &self.segment_secs)
            .field("retain_secs", &self.retain_secs)
            .field("retain_bytes", &self.retain_bytes)
            .field("writer_queue", &self.writer_queue)
            .field("header_kind", &self.header_kind)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_spec() {
        let c = BusConfig::default();
        assert!(c.dir.is_none());
        assert_eq!(c.segment_secs, 600);
        assert_eq!(c.retain_secs, 86_400);
        assert_eq!(c.retain_bytes, 5_000_000_000);
    }

    #[test]
    fn manual_clock_moves_only_when_told() {
        let c = ManualClock::new(1_000);
        assert_eq!((c.mono_ns(), c.wall_ms()), (0, 1_000));
        c.advance_secs(2);
        assert_eq!((c.mono_ns(), c.wall_ms()), (2_000_000_000, 3_000));
    }
}
