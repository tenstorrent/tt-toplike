// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Detect and monitor TT inference servers (Docker-first), reporting a lifecycle
//! state to the Insights screen. See
//! `docs/superpowers/specs/2026-07-02-tt-inference-server-monitoring-design.md`.

mod detect;
mod logs;
mod monitor;
mod probe;
mod services;
mod state;

pub use detect::{parse_inference_server, InferenceServer, Source};
pub use logs::last_non_health_line;
pub use monitor::{InferenceServerMonitor, CADENCE_SECS};
pub use probe::{
    count_lines, parse_docker_stats, parse_env_var, parse_liveness, top_process, ContainerProbe,
    DockerProbe, Readiness, TickSample,
};
pub use services::{service_for, ServiceDef, SERVERS};
pub use state::{estimate_progress, is_alarm, ModelProfile, Phase, ServiceState};
