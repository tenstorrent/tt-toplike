// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.


//! Error types for tt-toplike
//!
//! This module defines all error types used throughout the application.
//! We use thiserror for ergonomic error definition with automatic Display impl.

use std::io;
use thiserror::Error;

/// Main error type for tt-toplike
///
/// All errors in the application should eventually resolve to this type.
/// This provides a single, comprehensive error handling interface.
#[derive(Error, Debug)]
pub enum TTTopError {
    /// IO errors (file operations, subprocess communication)
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    /// JSON parsing/serialization errors
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Backend-specific errors (subprocess failures, device access issues)
    #[error("Backend error: {0}")]
    Backend(#[from] BackendError),

    /// Terminal/TUI errors (Ratatui, Crossterm issues)
    #[error("Terminal error: {0}")]
    Terminal(String),

    /// Configuration errors (invalid config files, missing required values)
    #[error("Configuration error: {0}")]
    Config(String),

    /// Generic error with context
    #[error("{0}")]
    Other(String),
}

/// Backend-specific error types
///
/// These errors occur when interacting with hardware backends (luwen or JSON subprocess).
#[derive(Error, Debug)]
pub enum BackendError {
    /// Backend initialization failed
    #[error("Initialization failed: {0}")]
    Initialization(String),

    /// Subprocess failed to start or crashed
    #[error("Subprocess error: {0}")]
    SubprocessFailed(String),

    /// Device not found or inaccessible
    #[error("Device not found: {0}")]
    DeviceNotFound(String),

    /// Telemetry read failed
    #[error("Telemetry read failed: {0}")]
    TelemetryFailed(String),

    /// Device driver not available or wrong version
    #[error("Driver error: {0}")]
    DriverError(String),

    /// Timeout waiting for telemetry update
    #[error("Timeout waiting for telemetry (waited {0}ms)")]
    Timeout(u64),

    /// Invalid telemetry data (unexpected format or values)
    #[error("Invalid telemetry data: {0}")]
    InvalidData(String),

    /// JSON parsing error (failed to parse tt-smi output)
    #[error("Parse error: {0}")]
    ParseError(String),

    /// Backend update failed
    #[error("Update failed: {0}")]
    Update(String),
}

/// Result type alias for convenience
///
/// Use this throughout the application instead of `Result<T, TTTopError>`
/// for cleaner function signatures.
pub type Result<T> = std::result::Result<T, TTTopError>;

/// Backend result type alias
pub type BackendResult<T> = std::result::Result<T, BackendError>;
