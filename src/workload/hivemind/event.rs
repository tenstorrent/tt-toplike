// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Normalized event model for the hivemindsweeper activity sniffer.

use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    Ttnn,
    TtMetal,
    TtLang,
    TtForge,
    TtXla,
    Vllm,
    TtSmi,
    Driver,
    Inspector,
    Host,
    Unknown,
}

impl Source {
    pub fn label(&self) -> &'static str {
        match self {
            Source::Ttnn => "ttnn",
            Source::TtMetal => "metal",
            Source::TtLang => "tt-lang",
            Source::TtForge => "tt-forge",
            Source::TtXla => "tt-xla",
            Source::Vllm => "vLLM",
            Source::TtSmi => "tt-smi",
            Source::Driver => "driver",
            Source::Inspector => "inspect",
            Source::Host => "host",
            Source::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Trace,
    Info,
    Notice,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Log,
    Compile,
    Process,
    Fd,
    DriverMsg,
    Emission,
    Inspector,
}

#[derive(Debug, Clone)]
pub struct SniffEvent {
    pub ts: Instant,
    pub source: Source,
    pub device: Option<u8>,
    pub severity: Severity,
    pub kind: EventKind,
    pub text: String,
    pub origin: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_orders_low_to_high() {
        assert!(Severity::Trace < Severity::Error);
        assert!(Severity::Warn < Severity::Error);
    }

    #[test]
    fn source_labels_are_stable() {
        assert_eq!(Source::TtMetal.label(), "metal");
        assert_eq!(Source::Host.label(), "host");
    }
}
