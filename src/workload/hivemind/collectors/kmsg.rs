// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Kernel ring-buffer (/dev/kmsg) collector: surfaces tt-kmd / PCIe driver
//! events that no userspace log carries. Read-only.

use crate::workload::hivemind::classify::classify;
use crate::workload::hivemind::event::{EventKind, Severity, SniffEvent, Source};
use std::time::Instant;

/// Parse one raw kmsg line into a driver SniffEvent, or None if not TT-relevant.
pub fn parse_kmsg_line(line: &str) -> Option<SniffEvent> {
    let low = line.to_lowercase();
    let relevant = low.contains("tenstorrent")
        || low.contains("tt_kmd")
        || low.contains("tt-kmd")
        || (low.contains("pcie") && (low.contains("aer") || low.contains("error")));
    if !relevant {
        return None;
    }
    // /dev/kmsg format: "prio,seq,ts,flag;message". Take the message half if present.
    let text = line.splitn(2, ';').nth(1).unwrap_or(line).trim().to_string();
    let severity = if low.contains("error") || low.contains("fail") || low.contains("aer") {
        Severity::Error
    } else if low.contains("warn") || low.contains("reset") {
        Severity::Warn
    } else {
        Severity::Notice
    };
    let device = parse_device_hint(&low);
    Some(SniffEvent {
        ts: Instant::now(),
        source: classify(None, &low), // → Driver for tenstorrent/tt_kmd; PCIe → Driver too below
        device,
        severity,
        kind: EventKind::DriverMsg,
        text,
        origin: "kmsg".into(),
    })
}

fn parse_device_hint(low: &str) -> Option<u8> {
    // Matches "tenstorrent!N", "card N", "device N".
    for key in ["tenstorrent!", "card ", "device "] {
        if let Some(idx) = low.find(key) {
            let rest = &low[idx + key.len()..];
            let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = num.parse::<u8>() {
                return Some(n);
            }
        }
    }
    None
}

use crate::workload::hivemind::collector::{Collector, Tx};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct KmsgCollector;

impl Collector for KmsgCollector {
    fn name(&self) -> &'static str {
        "kmsg"
    }
    fn run(&mut self, tx: Tx, shutdown: Arc<AtomicBool>) {
        // Open non-blocking so we can poll shutdown; fall back silently if denied.
        let file = match std::fs::File::open("/dev/kmsg") {
            Ok(f) => f,
            Err(_) => return, // engine marks PermDenied via its own probe (Task 9)
        };
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        while !shutdown.load(Ordering::SeqCst) {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => std::thread::sleep(std::time::Duration::from_millis(200)),
                Ok(_) => {
                    if let Some(ev) = parse_kmsg_line(&line) {
                        if tx.try_send(ev).is_err() {
                            // channel full: drop newest (engine counts drops)
                        }
                    }
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(200)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_irrelevant_lines() {
        assert!(parse_kmsg_line("6,100,12345,-;usb 1-1: new device").is_none());
    }

    #[test]
    fn parses_driver_reset_with_device_hint() {
        let ev =
            parse_kmsg_line("4,200,999,-;tenstorrent!1: watchdog reset issued").unwrap();
        assert_eq!(ev.source, Source::Driver);
        assert_eq!(ev.device, Some(1));
        assert_eq!(ev.severity, Severity::Warn);
        assert_eq!(ev.kind, EventKind::DriverMsg);
        assert!(ev.text.contains("watchdog reset"));
    }

    #[test]
    fn pcie_aer_is_error() {
        let ev = parse_kmsg_line("3,201,1000,-;pcie 0000:01:00.0: AER: corrected error")
            .unwrap();
        assert_eq!(ev.severity, Severity::Error);
    }
}
