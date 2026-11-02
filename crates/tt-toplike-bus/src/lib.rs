//! Signal bus for tt-toplike: the record types every source, the log, the
//! world reducer and every view share.
//!
//! It never touches hardware. Its I/O is the optional log directory and, on
//! Unix, the socket that serves the bus and the socket a client reads.
//!
//! * [`entity`] defines [`EntityKey`], the stable identity of a thing.
//! * [`record`] defines [`Record`] and its payloads, plus the JSON line format.
//! * [`config`] defines [`BusConfig`] and the [`Clock`] the bus reads.
//! * [`log`] defines [`Bus`]: publish, read by sequence number, subscribe.
//! * [`world`] defines [`World`], the reducer that holds the current state.
//! * [`signals`] names the signals sources and the world agree on.
//! * [`replay`] reads a recorded log back and republishes it.
//! * [`segment`] reads and writes the segment files on disk.
//! * [`inspect`] prints a summary of a recorded log (`--bus-inspect`).
//! * [`ident`] names the machine a bus runs on ([`host_identity`]).
//! * [`summary`] defines [`WorldSummary`], the serialisable part of the world.
//! * [`read`] defines [`WorldRead`], the read interface a view uses on a
//!   local world ([`LocalView`]) or a remote one (`RemoteView`).
//! * `socket` (Unix only) serves the bus on a Unix socket; `client` (Unix
//!   only) reads it, and its `RemoteWorld` keeps a remote world current.

#[cfg(unix)]
pub mod client;
pub mod config;
pub mod entity;
pub mod ident;
pub mod inspect;
pub mod log;
pub mod read;
pub mod record;
pub mod replay;
pub mod segment;
pub mod signals;
#[cfg(unix)]
pub mod socket;
pub mod summary;
#[cfg(test)]
mod testutil;
pub mod world;

#[cfg(unix)]
pub use client::{ClientError, Link, RemoteSubscription, RemoteView, RemoteWorld, SocketClient};
pub use config::{BusConfig, Clock, ManualClock, SystemClock};
pub use entity::{EntityKey, EntityKind};
pub use ident::host_identity;
pub use log::{install_quiet_reducer_hook, Bus, BusStats, ReadError, Reducer, Subscription};
pub use read::{LocalView, WorldRead};
pub use record::{
    DecodeError, Event, Gap, Header, HeaderKind, Payload, Provenance, RawFrame, Record, Sample,
    Sweep, Value, SCHEMA_VERSION,
};
pub use replay::{Hole, HoleKind, ManualPlayClock, PlayClock, RealClock, Replay};
#[cfg(unix)]
pub use socket::{ServerOptions, SocketServer};
pub use summary::{
    EventSummary, RawSummary, ReadingSummary, RemoteSnapshot, ResetSummary, WorldSummary,
    SUMMARY_EVENTS,
};
pub use world::{ModelPhase, ModelView, Reading, Snapshot, World};
