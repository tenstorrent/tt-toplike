//! Signal bus for tt-toplike: the record types every source, the log, the
//! world reducer and every view share.
//!
//! It never touches hardware. Its only I/O is the optional log directory.
//!
//! * [`entity`] defines [`EntityKey`], the stable identity of a thing.
//! * [`record`] defines [`Record`] and its payloads, plus the JSON line format.
//! * [`config`] defines [`BusConfig`] and the [`Clock`] the bus reads.
//! * [`log`] defines [`Bus`]: publish, read by sequence number, subscribe.
//! * [`segment`] reads and writes the segment files on disk.

pub mod config;
pub mod entity;
pub mod log;
pub mod record;
pub mod segment;
#[cfg(test)]
mod testutil;

pub use config::{BusConfig, Clock, ManualClock, SystemClock};
pub use entity::EntityKey;
pub use log::{Bus, BusStats, ReadError, Reducer, Subscription};
pub use record::{
    DecodeError, Event, Gap, Header, HeaderKind, Payload, Provenance, RawFrame, Record, Sample,
    Sweep, Value, SCHEMA_VERSION,
};
