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
//! * [`compat`] rebuilds the tt-station `/telemetry` frame and the inference
//!   monitor's `ServiceState` list from the world.
//! * [`gozer`] watches `gozer status --json` for chip leases and holds the
//!   `Governor` that lets a chip-opening source ask whether a chip is ours.
//! * [`query`] finds the entities of a kind that carry a set of signals.
//! * [`redact`] hides secrets in process command lines.
//! * [`proc_argv`] reads command lines and start times from `/proc` for the
//!   process source.
//! * [`host`] owns the bus, the identity map and every source, and drives
//!   them on the host's tick.
//! * [`headless`] runs a host with no screen (`--headless`): recording, the
//!   socket, and the `/telemetry` broadcast.
//! * [`view`] is what a view reads the bus through: a local host or an
//!   attached remote world, with a per-frame read that copies the world
//!   only when it changed.
//! * [`tui_bus`] is the TUI's side: host or attach at start-up, the data
//!   tick, `ViewChanged`, and the `/serve` frame.

pub mod compat;
pub mod gozer;
pub mod headless;
pub mod host;
pub mod identity;
pub mod proc_argv;
pub mod query;
pub mod redact;
pub mod signals;
pub mod source;
pub mod tui_bus;
pub mod view;
pub mod workload_sources;
