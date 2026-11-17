// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Helpers for testing a viz: render it to plain text and compare the text
//! with a golden file. Used by every plugin's tests; compiled into the library
//! (not just test builds) so plugin crates and examples can use it.

use super::{history::History, Viz, VizCtx};
use ratatui::{buffer::Buffer, layout::Rect};
use std::path::Path;
use tt_toplike_bus::{
    EntityKey, Event, HeaderKind, LocalView, Payload, Provenance, Record, Sample, Value, World,
    WorldRead,
};

/// Builds a small world for a viz test from public bus types (the bus's own
/// `testutil` is private to that crate).
///
/// ```ignore
/// let view = WorldBuilder::new()
///     .sample("0000:01:00.0", "power_w", 90.0)
///     .event(Event::Reset { chips: vec![] })
///     .build();
/// ```
///
/// Each call applies one record with the next sequence number. The clock is
/// one nanosecond per record, so readings are never stale at `build()`.
/// Reused by the declarative renderer's tests and by the example plugins.
#[derive(Default)]
pub struct WorldBuilder {
    world: World,
    seq: u64,
}

impl WorldBuilder {
    /// An empty world.
    pub fn new() -> Self {
        Self::default()
    }

    fn apply(mut self, payload: Payload) -> Self {
        self.seq += 1;
        self.world.apply(&Record {
            seq: self.seq,
            t_mono_ns: self.seq,
            t_wall_ms: self.seq,
            source: "test".into(),
            payload,
        });
        self
    }

    /// A numeric reading of `signal` on the chip at PCI address `bus_id`.
    pub fn sample(self, bus_id: &str, signal: &str, v: f64) -> Self {
        self.entity_sample(EntityKey::chip(bus_id), signal, Value::F64(v))
    }

    /// A reading of any value type on any entity.
    pub fn entity_sample(self, entity: EntityKey, signal: &str, value: Value) -> Self {
        self.apply(Payload::Sample(Sample {
            entity,
            signal: signal.into(),
            value,
            prov: Provenance::Measured,
        }))
    }

    /// A bus event.
    pub fn event(self, e: Event) -> Self {
        self.apply(Payload::Event(e))
    }

    /// The finished world, as the read view a `VizCtx` takes.
    pub fn build(self) -> LocalView {
        LocalView {
            seq: self.seq,
            world: self.world,
            now_mono_ns: self.seq.max(1),
            header_kind: HeaderKind::Live,
        }
    }
}

/// Render `viz` into a `w` x `h` buffer and return it as text: one line per
/// row, trailing spaces trimmed, rows joined with `\n`. Tick 0, and the
/// world's own clock.
pub fn render_to_text(
    viz: &mut dyn Viz,
    w: u16,
    h: u16,
    world: &dyn WorldRead,
    history: &History,
) -> String {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    let ctx = VizCtx {
        world,
        history,
        tick: 0,
        now_mono_ns: world.now_mono_ns(),
    };
    viz.render(&mut buf, area, &ctx);
    (0..h)
        .map(|y| {
            let row: String = (0..w).map(|x| buf[(x, y)].symbol()).collect();
            row.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Compare `actual` with the golden file at `path`, panicking with a diff
/// hint on a mismatch. With `UPDATE_GOLDEN=1` in the environment the file is
/// rewritten instead. A missing file fails and names that variable.
pub fn assert_golden(path: &Path, actual: &str) {
    let update = std::env::var("UPDATE_GOLDEN")
        .map(|v| v == "1")
        .unwrap_or(false);
    if let Err(msg) = check_golden(path, actual, update) {
        panic!("{msg}");
    }
}

/// The logic of [`assert_golden`] with `update` passed in, so tests need not
/// touch the environment.
fn check_golden(path: &Path, actual: &str, update: bool) -> Result<(), String> {
    if update {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        return std::fs::write(path, actual).map_err(|e| format!("{}: {e}", path.display()));
    }
    let want = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "golden file {} unreadable ({e}); run with UPDATE_GOLDEN=1 to create it",
            path.display()
        )
    })?;
    if want == actual {
        Ok(())
    } else {
        Err(format!(
            "golden mismatch for {}\n--- expected\n{want}\n--- actual\n{actual}\n(run with UPDATE_GOLDEN=1 to accept)",
            path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tt_toplike_bus::{HeaderKind, LocalView, World};

    /// Draws the chip count on row 1, indented, with trailing spaces.
    struct Count;
    impl Viz for Count {
        fn id(&self) -> &str {
            "count"
        }
        fn title(&self) -> &str {
            "Count"
        }
        fn render(&mut self, buf: &mut Buffer, area: Rect, ctx: &VizCtx) {
            let s = format!("  chips: {}   ", ctx.chips().len());
            buf.set_string(area.x, area.y + 1, s, ratatui::style::Style::default());
        }
        fn legend(&self) -> Vec<String> {
            Vec::new()
        }
        fn explain(&self) -> String {
            String::new()
        }
        fn help(&self) -> String {
            String::new()
        }
    }

    fn empty_view() -> LocalView {
        LocalView {
            seq: 0,
            world: World::new(),
            now_mono_ns: 0,
            header_kind: HeaderKind::Live,
        }
    }

    #[test]
    fn render_to_text_trims_trailing_spaces() {
        let text = render_to_text(&mut Count, 20, 3, &empty_view(), &History::default());
        assert_eq!(text, "\n  chips: 0\n");
    }

    #[test]
    fn golden_helper_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("frame.txt");
        // Missing file fails and names the variable.
        let err = check_golden(&path, "a", false).unwrap_err();
        assert!(err.contains("UPDATE_GOLDEN=1"), "{err}");
        // Update writes it; then it matches.
        check_golden(&path, "frame one", true).unwrap();
        check_golden(&path, "frame one", false).unwrap();
        // A changed string fails.
        assert!(check_golden(&path, "frame two", false).is_err());
    }
}
