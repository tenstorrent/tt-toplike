//! tt-station frame compatibility: the `/telemetry` frame built from the bus.
//!
//! Today a frame is the newest `tt-smi -s` snapshot with the `tt_toplike`
//! extension added (see `crate::backend::inject_extension`). The bus keeps
//! that snapshot verbatim as a `Raw` record from the source named
//! [`crate::bus::signals::RAW_SOURCE_TT_SMI`] (`"tt-smi"`, published by
//! `BackendSource`). These helpers rebuild the same frame from a [`World`].
//!
//! Nothing calls them yet. The two broadcast sites (the `/serve` block in
//! `ui/tui/mod.rs` and the headless loop in `bin/tui.rs`) switch over in
//! Task 10, once a host owns a `World`.
//!
//! Both helpers need the `remote` feature, because the extension types live
//! behind it.

use super::signals::RAW_SOURCE_TT_SMI;
use crate::backend::{inject_extension, TtToplikeExt};
use tt_toplike_bus::World;

/// The frame to broadcast for a box that reads its own chips.
///
/// Returns exactly `inject_extension(&raw.body, ext)` for the newest `tt-smi`
/// raw record in `world`, or `None` when the world has no such record yet.
/// A body that `inject_extension` cannot parse or that is not a JSON object
/// comes back unchanged, as it does there.
///
/// Relay rule that the Task 10 call site must keep: a `--remote` box
/// (`backend_type == BackendType::Remote`) re-broadcasts the origin's frame
/// verbatim and never adds its own extension. That call site must use
/// [`relay_frame`] and must not call this function.
pub fn telemetry_frame(world: &World, ext: &TtToplikeExt) -> Option<String> {
    let raw = world.latest_raw(RAW_SOURCE_TT_SMI)?;
    Some(inject_extension(&raw.body, ext))
}

/// The frame to re-broadcast for a `--remote` box: the newest `tt-smi` raw
/// body, byte for byte. No extension is added, so a `tt_toplike` key in the
/// result is the origin's own.
pub fn relay_frame(world: &World) -> Option<String> {
    world
        .latest_raw(RAW_SOURCE_TT_SMI)
        .map(|raw| raw.body.clone())
}

#[cfg(test)]
#[path = "compat_tests.rs"]
mod tests;
