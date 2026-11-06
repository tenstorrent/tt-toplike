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
//! Provenance: a model with a flag-text reading marked `Estimated` gets `~`
//! in front of its label, as Die 3D's flag text does. Both views call
//! [`model_text_is_estimated`]. That is every model of a
//! `--mock` host, and also a real host's model while it compiles or loads
//! with a progress figure, because the monitor's progress is an estimate
//! (Die 3D's flag starts with `~` for it too). The label is what the roster
//! line, the single-service header line and the creature's title all print,
//! so the marker shows wherever the model is named.
//!
//! A lost host: when the attached host's link is reconnecting or
//! disconnected, the view keeps the last world's models and marks them
//! stale ([`stale_note`], [`mark_stale`]): a `(reconnecting)` or
//! `(host disconnected)` line above the roster, and the roster drawn dim.
//! The creature below is not dimmed.
//!
//! The `v` rotation asks [`inference_active`], so an attached TUI on a box
//! with no models of its own still visits `i` while the host serves.

use crate::animation::die3d::overlay::model_text_is_estimated;
use crate::backend::TelemetryBackend;
use crate::bus::compat::world_to_service_states_with;
use crate::bus::tui_bus::TuiBus;
use crate::workload::inference_server::ServiceState;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
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
    // One row per model, in the same (key) order.
    for (s, m) in rows.iter_mut().zip(world.models()) {
        if model_text_is_estimated(world, &m) {
            s.label.insert(0, '~');
        }
    }
    rows
}

/// Whether the `i` view has something to show, for the `v` rotation's
/// dwell while another view is on screen: when attached, whether a model in
/// the attached world is not down (the rows the `i` view would draw);
/// otherwise `local()`, the local monitor's answer, which is only asked
/// then.
pub(crate) fn inference_active(
    bus: &TuiBus,
    backend: &dyn TelemetryBackend,
    local: impl FnOnce() -> bool,
) -> bool {
    match attached_rows(bus, backend) {
        Some(rows) => super::inference_rows_active(&rows),
        None => local(),
    }
}

/// The note for an attached host's link: `None` while connected.
#[cfg(unix)]
pub(crate) fn stale_note_for(link: tt_toplike_bus::Link) -> Option<&'static str> {
    match link {
        tt_toplike_bus::Link::Connected => None,
        tt_toplike_bus::Link::Reconnecting => Some("(reconnecting)"),
        tt_toplike_bus::Link::Disconnected => Some("(host disconnected)"),
    }
}

/// The note the `i` view shows when the attached host's link is lost, as of
/// the last data tick. `None` for a local bus and a connected host. The
/// models stay on screen: they are the last world received, and the note
/// says they are stale.
pub(crate) fn stale_note(bus: &TuiBus) -> Option<&'static str> {
    #[cfg(unix)]
    {
        bus.attached_link().and_then(stale_note_for)
    }
    #[cfg(not(unix))]
    {
        let _ = bus;
        None
    }
}

/// Mark the `i` view's roster stale: every line drawn dim, under a first
/// line carrying `note`. The note and the dim modifier are the style the
/// `--remote` fallback note already uses (secondary text, dim), so no new
/// colour is introduced. With no roster (a lone service), the note is the
/// only line. The caller must do this before counting the roster's height.
pub(crate) fn mark_stale(roster: &mut Vec<Line<'static>>, note: &'static str) {
    for line in roster.iter_mut() {
        line.style = line.style.add_modifier(Modifier::DIM);
        for span in line.spans.iter_mut() {
            span.style = span.style.add_modifier(Modifier::DIM);
        }
    }
    roster.insert(
        0,
        Line::from(Span::styled(
            note,
            Style::default()
                .fg(crate::ui::colors::text_secondary())
                .add_modifier(Modifier::DIM),
        )),
    );
}

#[cfg(test)]
#[path = "inference_bus_tests.rs"]
mod tests;
