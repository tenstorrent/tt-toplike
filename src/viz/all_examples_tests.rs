// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The examples-stay-alive test: every shipped viz is loaded the way the
//! program loads it and drawn at three sizes against two worlds.
//!
//! "Every shipped viz" is found, not listed:
//!
//! * Tier 1: every folder under `src/viz/plugins/`, through `viz::init()`
//!   (the registry entries whose source is `Folder`).
//! * Tier 2: every YAML file under the repository's `viz/` folder
//!   (`viz/` and `viz/examples/`), through the real [`Loader`].
//!
//! A new plugin folder or YAML example is therefore covered the day it is
//! added, and an edit that breaks one (a panic in `render`, a typo'd key that
//! stops a YAML file loading) turns this red. YAML files go into a private
//! registry so this test adds nothing to the global one other tests share.

use crate::viz::declarative::loader::Loader;
use crate::viz::registry::{self, is_valid_id, EntryStatus, Registry, Source, VizEntry};
use crate::viz::testkit::WorldBuilder;
use crate::viz::{History, VizCtx};
use ratatui::{buffer::Buffer, layout::Rect};
use std::collections::BTreeSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use tt_toplike_bus::{EntityKey, LocalView};

/// The terminal sizes every viz is drawn at: below, at and above typical.
const SIZES: [(u16, u16); 3] = [(40, 10), (80, 24), (120, 40)];

/// One shipped viz and how to run a closure on it.
enum Shipped {
    /// A Rust plugin in the global registry.
    Folder(VizEntry),
    /// A YAML file, in a private registry.
    Yaml(VizEntry),
}

/// Everything shipped, with the private YAML registry kept alive beside it.
struct All {
    yaml: Registry,
    entries: Vec<Shipped>,
}

fn entry(s: &Shipped) -> &VizEntry {
    match s {
        Shipped::Folder(e) | Shipped::Yaml(e) => e,
    }
}

/// Load both tiers. Panics (failing the test) when a YAML file does not load:
/// a shipped example that fails to load is exactly what this test is for.
fn load_all() -> All {
    let _ = crate::viz::init();
    // The ids the plugin folders on disk must have. Other tests register
    // stand-ins in the global registry with a folder source; filtering on the
    // folder names keeps this test to the real plugins, and checks that each
    // folder did register.
    let plugins = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/viz/plugins");
    let folder_ids: BTreeSet<String> = std::fs::read_dir(&plugins)
        .expect("src/viz/plugins exists")
        .flatten()
        .filter(|d| d.path().join("mod.rs").is_file())
        .map(|d| d.file_name().to_string_lossy().replace('_', "-"))
        .collect();
    let mut entries: Vec<Shipped> = registry::entries()
        .into_iter()
        .filter(|e| e.source == Source::Folder && folder_ids.contains(&e.id))
        .map(Shipped::Folder)
        .collect();
    assert_eq!(
        entries.len(),
        folder_ids.len(),
        "plugin folders {folder_ids:?} did not all register"
    );

    let mut yaml = Registry::new();
    let viz_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("viz");
    Loader::new(vec![viz_dir]).scan_with(&mut yaml);
    for e in yaml.entries() {
        if let EntryStatus::Failed(why) = &e.status {
            panic!("shipped YAML viz `{}` failed to load: {why}", e.id);
        }
        entries.push(Shipped::Yaml(e));
    }
    assert!(
        entries.iter().any(|e| matches!(e, Shipped::Yaml(_))),
        "no YAML viz found under viz/: the scan found nothing to check"
    );
    assert!(
        entries.iter().any(|e| matches!(e, Shipped::Folder(_))),
        "no Rust plugin folder registered: the scan found nothing to check"
    );
    All { yaml, entries }
}

/// Three chips, each with a short rising history of power and temperature.
fn fixture_world() -> (LocalView, History) {
    let mut world = WorldBuilder::new();
    let mut history = History::default();
    for (bus, power, temp) in [
        ("0000:01:00.0", 60.0, 50.0),
        ("0000:02:00.0", 95.5, 75.0),
        ("0000:03:00.0", 270.0, 90.0),
    ] {
        world =
            world
                .sample(bus, "tt_tdp_w", power)
                .sample(bus, "tt_asic_temperature_celsius", temp);
        for i in 0..12 {
            let key = EntityKey::chip(bus);
            history.record(&key, "tt_tdp_w", power - 12.0 + i as f64);
            history.record(
                &key,
                "tt_asic_temperature_celsius",
                temp - 6.0 + (i / 2) as f64,
            );
        }
    }
    (world.build(), history)
}

#[test]
fn every_example_renders_at_three_sizes_without_panicking() {
    let all = load_all();
    let empty = (WorldBuilder::new().build(), History::default());
    let worlds = [("empty world", &empty), ("fixture world", &fixture_world())];

    for shipped in &all.entries {
        let e = entry(shipped);
        for (label, (view, history)) in worlds.iter() {
            for (w, h) in SIZES {
                let area = Rect::new(0, 0, w, h);
                let mut buf = Buffer::empty(area);
                let ctx = VizCtx {
                    world: view,
                    history,
                    tick: 3,
                    now_mono_ns: view.now_mono_ns,
                };
                let draw = |v: &mut dyn crate::viz::Viz| v.render(&mut buf, area, &ctx);
                // catch_unwind only to name the viz, size and world in the
                // failure; the panic still fails the test.
                let result = catch_unwind(AssertUnwindSafe(|| match shipped {
                    Shipped::Folder(_) => registry::with_viz(e.handle, draw),
                    Shipped::Yaml(_) => all.yaml.with_viz(e.handle, draw),
                }));
                match result {
                    Ok(Some(())) => {}
                    Ok(None) => panic!("viz `{}` is registered but has no instance", e.id),
                    Err(_) => panic!("viz `{}` panicked drawing {w}x{h} on the {label}", e.id),
                }
            }
        }
    }
}

#[test]
fn ids_are_unique_and_kebab_case_across_both_tiers() {
    let all = load_all();
    let mut seen = BTreeSet::new();
    for shipped in &all.entries {
        let e = entry(shipped);
        assert!(is_valid_id(&e.id), "id `{}` is not kebab-case", e.id);
        assert!(
            e.id.starts_with(|c: char| c.is_ascii_lowercase()) && e.id.len() <= 48,
            "id `{}` must start with a letter and be at most 48 characters",
            e.id
        );
        assert!(
            seen.insert(e.id.clone()),
            "id `{}` is used by more than one shipped viz (Rust folder and YAML file share one namespace)",
            e.id
        );
    }
}
