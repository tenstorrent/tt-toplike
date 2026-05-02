// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Workload detection, process monitoring, and inference classification.

#[cfg(feature = "linux-procfs")]
pub mod process_monitor;
pub mod inference;

#[cfg(feature = "linux-procfs")]
pub use process_monitor::{ProcessInfo, ProcessMonitor};
pub use inference::{
    Confidence, DeviceInferenceState, InferenceEngine, InferenceResult, PowerTrend,
    TelemetrySample, state_label, state_color,
};
