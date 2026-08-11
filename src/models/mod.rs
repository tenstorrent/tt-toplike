// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Data models for Tenstorrent hardware telemetry
//!
//! This module contains all data structures representing hardware information,
//! telemetry data, and device state. These models are designed to be compatible
//! with both luwen (direct hardware) and JSON (subprocess) backends.

pub mod device;
pub(crate) mod serde_num;
pub mod telemetry;

// Re-export commonly used types
pub use device::{Architecture, Device};
pub use telemetry::{
    fw_bundle_version_string, tensix_col_harvested, unpack_gddr_temps, DeviceLimits, DeviceProcess,
    FirmwaresInfo, GddrTempPair, SmbusTelemetry, Telemetry, BH_TENSIX_COL_COUNT,
};
