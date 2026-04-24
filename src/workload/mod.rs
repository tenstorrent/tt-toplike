// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.


//! Workload detection and process monitoring
//!
//! This module provides functionality to detect which processes are using
//! Tenstorrent hardware devices. It scans the `/proc` filesystem to find
//! processes with open file descriptors to `/dev/tenstorrent/*` devices
//! and processes using hugepages or Tenstorrent-related memory mappings.

#[cfg(feature = "linux-procfs")]
pub mod process_monitor;

#[cfg(feature = "linux-procfs")]
pub use process_monitor::{ProcessInfo, ProcessMonitor};
