//! Entity identity.
//!
//! An [`EntityKey`] names a thing that outlives a single sample: a chip, a
//! tile, a model, a process. It is built from stable identifiers (PCI address,
//! board serial, NoC0 coordinates, pid plus start time) and never from a list
//! position, because `/dev/tenstorrent/N` indexes can change between runs.
//!
//! The key is a plain string of the form `kind:id`, with child entities
//! appending to their parent's key (`tile:chip:0000:03:00.0/1,2`).

use serde::{Deserialize, Serialize};
use std::fmt;

/// Stable identity of an entity. Serialises as a bare JSON string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntityKey(String);

impl EntityKey {
    /// A chip, keyed by PCI address (`Device.bus_id`).
    pub fn chip(bus_id: &str) -> Self {
        Self(format!("chip:{bus_id}"))
    }
    /// A board, keyed by serial number.
    pub fn board(serial: &str) -> Self {
        Self(format!("board:{serial}"))
    }
    /// A tile on a chip, keyed by chip key plus NoC0 coordinates.
    pub fn tile(chip: &EntityKey, noc0_x: u8, noc0_y: u8) -> Self {
        Self(format!("tile:{}/{noc0_x},{noc0_y}", chip.0))
    }
    /// A served model, keyed by its service key.
    pub fn model(key: &str) -> Self {
        Self(format!("model:{key}"))
    }
    /// A process. The start time makes a recycled pid a different entity.
    pub fn process(pid: u32, start_ticks: u64) -> Self {
        Self(format!("process:{pid}@{start_ticks}"))
    }
    /// A chip lease, keyed by lease id.
    pub fn lease(id: &str) -> Self {
        Self(format!("lease:{id}"))
    }
    /// A host, keyed by name.
    pub fn host(name: &str) -> Self {
        Self(format!("host:{name}"))
    }
    /// A DRAM channel on a chip.
    pub fn dram_channel(chip: &EntityKey, ch: u8) -> Self {
        Self(format!("dram:{}/{ch}", chip.0))
    }
    /// An Ethernet port on a chip.
    pub fn eth_port(chip: &EntityKey, port: u8) -> Self {
        Self(format!("eth:{}/{port}", chip.0))
    }
    /// A training run, keyed by its run key.
    pub fn training_run(key: &str) -> Self {
        Self(format!("run:{key}"))
    }
    /// The key as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EntityKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
