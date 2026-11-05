// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The plugin trait and the context a plugin renders with.

use super::history::History;
use ratatui::{buffer::Buffer, layout::Rect};
use tt_toplike_bus::{EntityKey, EntityKind, WorldRead};

/// One signal a viz reads, declared so the host (and the docs) can list what
/// a plugin depends on.
///
/// Returned by [`Viz::signals`]; the host records a history for each one
/// while the viz is on screen (see [`History`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalSpec {
    /// The kind of entity the signal is read from.
    pub kind: EntityKind,
    /// The signal name on the bus, for example `"asic_temp"`.
    pub name: String,
}

/// What a viz sees on one render: the world, the history the host keeps, and
/// the clock.
///
/// Built by the host once per frame (and by [`super::testkit`] in tests) and
/// passed to [`Viz::render`] by reference.
pub struct VizCtx<'a> {
    /// Read access to the bus world.
    pub world: &'a dyn WorldRead,
    /// Recent values of the signals the host records.
    pub history: &'a History,
    /// The host's data-tick counter: it counts data ticks (the same clock the
    /// history cadence uses), not drawn frames.
    pub tick: u64,
    /// The bus clock's monotonic time in nanoseconds; the scale every
    /// reading's `t_mono_ns` is on.
    pub now_mono_ns: u64,
}

impl VizCtx<'_> {
    /// Every chip the world holds a reading for, in key order.
    ///
    /// A convenience for plugins; it is `world.entities(EntityKind::Chip)`.
    pub fn chips(&self) -> Vec<EntityKey> {
        self.world.entities(EntityKind::Chip)
    }
}

/// A full-screen view supplied as a plugin.
///
/// The host (the TUI) owns one boxed instance per registered plugin and calls
/// it only from the draw thread, so it needs `Send` but not `Sync`.
///
/// A viz runs inside `registry::with_viz`, under its own mutex. Calling other
/// registry functions from a method is fine. Re-entering `with_viz` on the
/// same handle from a method deadlocks.
pub trait Viz: Send {
    /// The stable kebab-case id (`[a-z0-9]+(-[a-z0-9]+)*`). The registry keys
    /// on it and `--viz <id>` and `[viz] enable` name it.
    fn id(&self) -> &str;

    /// The name shown in the view cycle and the title bar.
    fn title(&self) -> &str;

    /// The smallest terminal (width, height) the viz can draw in. The host
    /// shows a "needs W x H" message below it instead of calling `render`.
    fn min_size(&self) -> (u16, u16) {
        (40, 10)
    }

    /// The signals the viz reads. The host records a history for exactly these
    /// signals while the viz is on screen (see [`History`]); the default is none.
    fn signals(&self) -> Vec<SignalSpec> {
        Vec::new()
    }

    /// Draw the view into `area` of `buf`. Called every frame by the host.
    fn render(&mut self, buf: &mut Buffer, area: Rect, ctx: &VizCtx);

    /// Lines for the legend panel, shown by the host on request.
    fn legend(&self) -> Vec<String>;

    /// A plain-language account of what the view shows, for the explain panel.
    fn explain(&self) -> String;

    /// Help text for the help panel. A viz has no keys of its own, so this
    /// describes the view and its settings.
    fn help(&self) -> String;

    /// Receive the viz's `[viz.<id>]` table from the config file. Called by
    /// the host once at start-up, and again when a YAML viz file is reloaded
    /// (a Rust plugin is built once, so for it only the first). The default
    /// ignores it.
    fn configure(&mut self, cfg: &toml_edit::Table) {
        let _ = cfg;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tt_toplike_bus::{
        HeaderKind, LocalView, Payload, Provenance, Record, Sample, Value, World,
    };

    /// `chips()` lists a chip the world holds a reading for.
    #[test]
    fn chips_lists_a_chip_with_a_reading() {
        let chip = EntityKey::chip("0000:01:00.0");
        let mut world = World::new();
        world.apply(&Record {
            seq: 1,
            t_mono_ns: 1,
            t_wall_ms: 1,
            source: "test".into(),
            payload: Payload::Sample(Sample {
                entity: chip.clone(),
                signal: "power".into(),
                value: Value::F64(42.0),
                prov: Provenance::Measured,
            }),
        });
        let view = LocalView {
            seq: 1,
            world,
            now_mono_ns: 1,
            header_kind: HeaderKind::Live,
        };
        let history = History::default();
        let ctx = VizCtx {
            world: &view,
            history: &history,
            tick: 0,
            now_mono_ns: 1,
        };
        assert_eq!(ctx.chips(), vec![chip]);
    }
}
