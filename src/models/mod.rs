// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Data models for Tenstorrent hardware telemetry
//!
//! This module contains all data structures representing hardware information,
//! telemetry data, and device state. These models are designed to be compatible
//! with both luwen (direct hardware) and JSON (subprocess) backends.

pub mod device;
pub mod telemetry;

// Re-export commonly used types
pub use device::{Architecture, Device};
pub use telemetry::{
    tensix_col_harvested, unpack_gddr_temps, DeviceLimits, FirmwaresInfo, GddrTempPair,
    SmbusTelemetry, Telemetry, BH_TENSIX_COL_COUNT,
};
