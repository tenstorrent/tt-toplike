// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests for the Rust plugin folders found by `build.rs`.
//!
//! These use the global registry, so they only look at entries from plugin
//! folders and never assume a count of other entries. They pass with zero
//! plugin folders.

use super::registry::{entries, handle_of, Source};

/// The kebab-case id a folder name must produce: `_` becomes `-`.
fn id_for_folder(name: &str) -> String {
    name.replace('_', "-")
}

#[test]
fn every_plugin_folder_id_matches_its_folder_name() {
    // `init` is idempotent, so calling it here is safe next to other tests.
    let _ = super::init();

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/viz/plugins");
    for item in std::fs::read_dir(&dir).expect("src/viz/plugins exists") {
        let path = item.expect("dir entry").path();
        if !path.join("mod.rs").is_file() {
            continue; // not a plugin folder
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let id = id_for_folder(&name);
        let handle = handle_of(&id)
            .unwrap_or_else(|| panic!("folder `{name}` is not registered under id `{id}`"));
        let entry = entries()
            .into_iter()
            .find(|e| e.handle == handle)
            .expect("entry for handle");
        assert_eq!(entry.source, Source::Folder, "folder `{name}`");
        assert_eq!(entry.id, id, "folder `{name}`");
    }
}

#[test]
fn init_is_idempotent() {
    let _ = super::init();
    // Other tests register and remove `Source::Folder` stand-ins in the same
    // global registry on other threads, so only the plugin folders that exist
    // on disk are compared (as `all_examples_tests` does).
    let on_disk: std::collections::BTreeSet<String> =
        std::fs::read_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/viz/plugins"))
            .expect("src/viz/plugins exists")
            .flatten()
            .filter(|d| d.path().join("mod.rs").is_file())
            .map(|d| d.file_name().to_string_lossy().replace('_', "-"))
            .collect();
    let folder_ids = || {
        let mut v: Vec<String> = entries()
            .into_iter()
            .filter(|e| e.source == Source::Folder && on_disk.contains(&e.id))
            .map(|e| e.id)
            .collect();
        v.sort();
        v
    };
    let before = folder_ids();
    assert_eq!(before.len(), on_disk.len(), "every folder registered");
    let again = super::init();
    assert!(again.is_empty(), "second init reported errors: {again:?}");
    assert_eq!(before, folder_ids(), "second init registered something new");
}
