// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Workload detection, process monitoring, and inference classification.

pub mod inference;
#[cfg(feature = "linux-procfs")]
pub mod process_monitor;
#[cfg(feature = "linux-procfs")]
pub mod serving;

pub use inference::{
    state_color, state_label, Confidence, DeviceInferenceState, InferenceEngine, InferenceResult,
    PowerTrend, TelemetrySample,
};
#[cfg(feature = "linux-procfs")]
pub use process_monitor::{ProcessInfo, ProcessMonitor};
#[cfg(feature = "linux-procfs")]
pub use serving::{InferenceServerProbe, ServerFlavour, ServingMetrics};
