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

pub mod ctx;
pub mod history;
pub mod registry;
pub mod testkit;

#[cfg(test)]
mod history_tests;
#[cfg(test)]
mod registry_tests;

pub use ctx::{SignalSpec, Viz, VizCtx};
pub use history::History;
pub use registry::{EntryStatus, RegisterError, Registry, Source, VizEntry, VizHandle};
