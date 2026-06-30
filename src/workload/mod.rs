// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Workload detection, process monitoring, and inference classification.

pub mod host_processes;
pub mod inference;
pub mod inference_match;
pub mod liveness_probe;
#[cfg(all(target_os = "linux", feature = "linux-procfs"))]
pub mod process_monitor;
#[cfg(all(target_os = "linux", feature = "linux-procfs"))]
pub mod serving;

pub use host_processes::{HostProcessMonitor, ProcRow, TtProcInfo};
pub use inference::{
    state_color, state_label, Confidence, DeviceInferenceState, InferenceEngine, InferenceResult,
    PowerTrend, TelemetrySample,
};
pub use inference_match::inference_match;
pub use liveness_probe::{DetectedRuntime, LivenessProber};
#[cfg(all(target_os = "linux", feature = "linux-procfs"))]
pub use process_monitor::{ProcessInfo, ProcessMonitor};
#[cfg(all(target_os = "linux", feature = "linux-procfs"))]
pub use serving::{InferenceServerProbe, ServerFlavour, ServingMetrics};
