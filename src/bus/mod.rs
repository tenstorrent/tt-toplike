//! The signal bus inside the main crate: the adapters that turn tt-toplike's
//! existing data sources into bus records.
//!
//! The record types, the log and the world live in the `tt_toplike_bus`
//! library crate. This module holds what needs the main crate's own types.
//!
//! * [`identity`] maps `/dev/tenstorrent/N` indexes to chip keys.

pub mod identity;
