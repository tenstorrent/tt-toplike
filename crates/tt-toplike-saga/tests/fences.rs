//! Fence tests: the saga core must stay pure and deterministic.
//!
//! Two files carry the fence. `Cargo.toml` limits what the crate can link
//! against, and `clippy.toml` bans the calls and types that would let a wall
//! clock, the environment or hash-map iteration order leak into a decision.
//! These tests read both files from disk, so a careless edit that loosens
//! either one fails here. Tests may use `std::fs`; the clippy config only bans
//! what the plan lists for the library itself.

use std::fs;
use std::path::PathBuf;

/// Read a file that sits in the crate directory (next to `Cargo.toml`).
fn read_crate_file(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Names of the crates listed under `[dependencies]` and under any
/// target-specific `[target.*.dependencies]` table. Dev and build
/// dependencies are not part of the library's link set and are not fenced.
fn runtime_dependencies(manifest: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_runtime_table = false;
    for raw in manifest.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            // A table header. Runtime tables end in `dependencies` and carry no
            // `dev-` or `build-` prefix.
            let header = line.trim_matches(|c| c == '[' || c == ']');
            in_runtime_table = header == "dependencies"
                || (header.starts_with("target.") && header.ends_with(".dependencies"));
            continue;
        }
        if !in_runtime_table || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, _)) = line.split_once('=') {
            names.push(name.trim().to_string());
        }
    }
    names.sort();
    names
}

#[test]
fn dependencies_are_the_bus_crate_and_at_most_serde() {
    let manifest = read_crate_file("Cargo.toml");
    let deps = runtime_dependencies(&manifest);
    assert!(
        deps.contains(&"tt-toplike-bus".to_string()),
        "the core reads bus records, so it must depend on tt-toplike-bus: {deps:?}"
    );
    for dep in &deps {
        assert!(
            dep == "tt-toplike-bus" || dep == "serde",
            "the core may depend only on tt-toplike-bus and serde, found `{dep}`"
        );
    }
}

#[test]
fn clippy_config_bans_the_nondeterministic_calls_and_types() {
    let config = read_crate_file("clippy.toml");
    for path in [
        "std::time::Instant::now",
        "std::time::SystemTime::now",
        "std::env::var",
        "std::collections::HashMap",
        "std::collections::HashSet",
    ] {
        assert!(
            config.contains(&format!("\"{path}\"")),
            "clippy.toml must ban `{path}`"
        );
    }
    assert!(
        config.contains("disallowed-methods"),
        "missing disallowed-methods"
    );
    assert!(
        config.contains("disallowed-types"),
        "missing disallowed-types"
    );
}

#[test]
fn lib_forbids_unsafe_code() {
    let lib = read_crate_file("src/lib.rs");
    assert!(
        lib.contains("#![forbid(unsafe_code)]"),
        "src/lib.rs must forbid unsafe code"
    );
}
