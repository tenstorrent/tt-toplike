// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Live tt-train run monitoring: detection, log parsing, config reading.
//!
//! Split follows `inference_server/`: pure logic here and in `parse`/`config`,
//! all I/O confined to `monitor`.

pub mod detect;

pub use detect::{parse_train_process, TrainProcess, TRAIN_BINARIES};
