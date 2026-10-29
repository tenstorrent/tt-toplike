// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Die 3D view: an isometric, pixel-rendered picture of the chip die.
//!
//! The scene is drawn into an RGB pixel buffer (`raster`) and later blitted to
//! the terminal as half-block cells. Further sub-modules are added by later
//! tasks of the plan in `docs/superpowers/plans/2026-10-29-die-3d-view.md`.

pub mod raster;
pub mod cells;
