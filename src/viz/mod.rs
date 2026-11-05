// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Viz plugins: the platform that lets a full-screen view be added as a
//! folder (Rust) or a single file (YAML) without touching shared code.
//!
//! This module holds the parts every plugin and the host share:
//!
//! * [`Viz`], the trait a plugin implements, with [`SignalSpec`];
//! * [`VizCtx`], what a plugin is handed on each render;
//! * [`History`], a capped per-signal series the host feeds each tick;
//! * [`registry`], the process-wide list of plugins, keyed by a stable
//!   [`VizHandle`];
//! * [`testkit`], helpers that render a viz to text and compare it with a
//!   golden file.
//!
//! The built-in views do not use any of this; they stay on `DisplayMode`.

pub mod cli;
pub mod ctx;
pub mod declarative;
pub mod history;
pub mod plugins;
pub mod registry;
pub mod testkit;

#[cfg(test)]
mod all_examples_tests;
#[cfg(test)]
mod cli_tests;
#[cfg(test)]
mod history_tests;
#[cfg(test)]
mod plugins_tests;
#[cfg(test)]
mod registry_tests;

pub use ctx::{SignalSpec, Viz, VizCtx};
pub use history::History;
pub use registry::{EntryStatus, RegisterError, Registry, Source, VizEntry, VizHandle};

/// Register every Rust plugin folder, once per process.
///
/// The folders are found at build time (see `build.rs` and
/// `src/viz/plugins/README.md`). The first call registers them and returns
/// the errors, if any, for the caller to log; later calls do nothing and
/// return an empty list. YAML plugins are loaded separately and are not part
/// of this call.
pub fn init() -> Vec<RegisterError> {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    let mut errors = Vec::new();
    ONCE.call_once(|| errors = plugins::register_all());
    errors
}
