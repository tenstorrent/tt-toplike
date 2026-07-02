// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Detect and monitor TT inference servers (Docker-first), reporting a lifecycle
//! state to the Insights screen. See
//! `docs/superpowers/specs/2026-07-02-tt-inference-server-monitoring-design.md`.

mod detect;

pub use detect::{parse_inference_server, InferenceServer, Source};
