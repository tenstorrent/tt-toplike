//! Where the `i` view (the inference monitor) gets its service rows.
//!
//! * A TUI that hosts its own bus (the normal case) keeps the local
//!   inference monitor's `ServiceState` list exactly as before. Sending the
//!   local monitor through the bus and back would lose fields the bus does
//!   not carry (`last_log`, `top_proc`, the port, ...) and gain nothing.
//! * A TUI attached to a headless host (`--bus-attach`, or the start-up
//!   probe) has no inference monitor of its own worth showing: the host
//!   runs the monitor. The `i` view then draws the rows
//!   [`crate::bus::compat::world_to_service_states_with`] builds from the
//!   attached world.
//!
//! The attached world is read with [`TuiBus::read_unfiltered`]: the `i`
//! view lists every model the host knows, also those on chips this TUI's
//! backend does not have. Die 3D keeps the chip limit ([`TuiBus::read`]).
//!
//! Chip indexes: the world names chips by PCI address. A chip key turns
//! into a `/dev/tenstorrent/N` index through this TUI's own backend device
//! list, and only when the attached host's origin matches this machine
//! ([`TuiBus::origin_is_local`]). A host on another machine, a mock host or
//! a replay gives rows with no chips, because its PCI addresses are not
//! this machine's. A key with no local device is left out.
//!
//! Provenance: a model with a shown reading marked `Estimated` gets `~` in
//! front of its label, as Die 3D's flag text does
//! ([`crate::bus::compat::model_is_estimated`]). That is every model of a
//! `--mock` host, and also a real host's model while it compiles or loads
//! with a progress figure, because the monitor's progress is an estimate
//! (Die 3D's flag starts with `~` for it too). The label is what the roster
//! line, the single-service header line and the creature's title all print,
//! so the marker shows wherever the model is named.

use crate::backend::TelemetryBackend;
use crate::bus::compat::{model_is_estimated, world_to_service_states_with};
use crate::bus::tui_bus::TuiBus;
use crate::workload::inference_server::ServiceState;
use std::collections::HashMap;
use tt_toplike_bus::{EntityKey, WorldRead};

/// The `i` view's rows from the attached world, or `None` when the TUI
/// hosts its own bus and the caller keeps the local monitor's rows. See the
/// module doc.
pub(crate) fn attached_rows(
    bus: &TuiBus,
    backend: &dyn TelemetryBackend,
) -> Option<Vec<ServiceState>> {
    if !bus.is_attached() {
        return None;
    }
    let world = bus.read_unfiltered();
    let local: HashMap<EntityKey, usize> = if bus.origin_is_local() {
        backend
            .devices()
            .iter()
            .filter(|d| !d.bus_id.is_empty())
            .map(|d| (EntityKey::chip(&d.bus_id), d.index))
            .collect()
    } else {
        HashMap::new()
    };
    Some(rows_from_world(&world, &|k| local.get(k).copied()))
}

/// The rows for `world`, with chip keys resolved by `chip_index` and `~`
/// before the label of each estimated model.
pub(crate) fn rows_from_world(
    world: &dyn WorldRead,
    chip_index: &dyn Fn(&EntityKey) -> Option<usize>,
) -> Vec<ServiceState> {
    let mut rows = world_to_service_states_with(world, chip_index);
    for s in &mut rows {
        if model_is_estimated(world, &EntityKey::model(&s.key)) {
            s.label.insert(0, '~');
        }
    }
    rows
}

#[cfg(test)]
#[path = "inference_bus_tests.rs"]
mod tests;
