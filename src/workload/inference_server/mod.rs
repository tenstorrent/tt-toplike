// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Detect and monitor TT inference servers (Docker-first), reporting a lifecycle
//! state to the Insights screen. See
//! `docs/superpowers/specs/2026-07-02-tt-inference-server-monitoring-design.md`.

// `detect` and `probe` are `pub(crate)` (not private) so `workload::hivemind`'s
// `log_tail` collector can reuse the same container-recognition/inspect parsing
// and the hardened, timeout-guarded `docker()` shell-out helper, rather than
// duplicating docker-CLI plumbing. Neither is re-exported outside the crate.
pub(crate) mod detect;
pub mod education;
mod logs;
pub mod metrics;
mod monitor;
pub(crate) mod probe;
mod services;
mod state;

pub use detect::{parse_direct_vllm, parse_inference_server, service_key, InferenceServer, Source};
pub use logs::last_non_health_line;
pub use metrics::{
    parse_media_metrics, parse_vllm_metrics, MediaCounters, MediaStats, ServingStats, VllmCounters,
};
pub use monitor::{InferenceServerMonitor, CADENCE_SECS};
pub use probe::{
    count_lines, parse_docker_stats, parse_env_var, parse_liveness, top_process, ContainerProbe,
    DockerProbe, Readiness, TickSample,
};
pub use services::{service_for, ServiceDef, SERVERS};
pub use state::{estimate_progress, is_alarm, ModelProfile, Phase, ServiceState};
