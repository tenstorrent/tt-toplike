// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Rust viz plugins, one folder each. See `README.md` in this folder for the
//! rules a plugin folder follows.
//!
//! `build.rs` lists the folders next to this file and writes
//! `$OUT_DIR/viz_plugins.rs`: a `pub mod` line for each folder and
//! `register_all()`, which registers them. Adding a folder therefore edits no
//! shared file.

include!(concat!(env!("OUT_DIR"), "/viz_plugins.rs"));
