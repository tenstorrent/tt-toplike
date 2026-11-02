//! Signal bus for tt-toplike: the record types every source, the log, the
//! world reducer and every view share.
//!
//! This crate holds data shapes only. It has no I/O and never touches
//! hardware.
//!
//! * [`entity`] defines [`EntityKey`], the stable identity of a thing.
//! * [`record`] defines [`Record`] and its payloads, plus the JSON line format.

pub mod entity;
pub mod record;

pub use entity::EntityKey;
pub use record::{
    DecodeError, Event, Gap, Header, HeaderKind, Payload, Provenance, RawFrame, Record, Sample,
    Sweep, Value, SCHEMA_VERSION,
};
