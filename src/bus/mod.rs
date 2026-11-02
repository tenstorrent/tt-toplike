//! The signal bus inside the main crate: the adapters that turn tt-toplike's
//! existing data sources into bus records.
//!
//! The record types, the log and the world live in the `tt_toplike_bus`
//! library crate. This module holds what needs the main crate's own types.
//!
//! * [`identity`] maps `/dev/tenstorrent/N` indexes to chip keys.
//! * [`signals`] names the signals the backend source writes.
//! * [`source`] defines the `Source` trait and `BackendSource`.
//! * [`workload_sources`] turns the workload monitors' snapshots (inference,
//!   training, hivemind, processes, `--remote`) into records.
//! * [`gozer`] watches `gozer status --json` for chip leases and holds the
//!   `Governor` that lets a chip-opening source ask whether a chip is ours.
//! * [`redact`] hides secrets in process command lines.

pub mod gozer;
pub mod identity;
pub mod redact;
pub mod signals;
pub mod source;
pub mod workload_sources;
