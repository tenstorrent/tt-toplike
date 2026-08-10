// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! PCIe data-word counters from tt-kmd's sysfs class attributes.
//!
//! tt-kmd exposes `/sys/class/tenstorrent/tenstorrent!N/pcie_perf_counters/`
//! — 12 world-readable, monotonically increasing counters of 32-bit data
//! words crossing the PCIe link, split by direction and initiator:
//!
//! * `mst_*` — the DEVICE is the PCIe master (device-initiated DMA):
//!   `mst_rd_data_word_received*` = device reading host memory (→ into chip),
//!   `mst_{posted,nonposted}_wr_data_word_sent*` = device writing host memory
//!   (→ out of chip).
//! * `slv_*` — the device is the PCIe target (host-initiated MMIO/DMA):
//!   `slv_{posted,nonposted}_wr_data_word_received*` = host writing to the
//!   device (→ into chip), `slv_rd_data_word_sent*` = host reading from the
//!   device (→ out of chip).
//!
//! Each name has a `0` and `1` suffix (two PCIe controllers); both are summed.
//! Reading these files is passive — no device open, no ARC message.

use std::fs;
use std::path::Path;
use std::time::Instant;

/// Bytes per 32-bit PCIe data word.
const BYTES_PER_WORD: u64 = 4;

/// Raw cumulative word counts, folded down to the two directions we render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcieCounterSnapshot {
    /// Words that entered the chip (host writes + device reads of host memory).
    pub words_in: u64,
    /// Words that left the chip (host reads + device writes to host memory).
    pub words_out: u64,
}

/// Derived link bandwidth between two snapshots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PcieBandwidth {
    /// Bytes/second flowing INTO the chip.
    pub rx_bytes_per_sec: f64,
    /// Bytes/second flowing OUT of the chip.
    pub tx_bytes_per_sec: f64,
}

/// Counter files whose sum is "data into the chip".
const IN_COUNTERS: [&str; 6] = [
    "slv_posted_wr_data_word_received0",
    "slv_posted_wr_data_word_received1",
    "slv_nonposted_wr_data_word_received0",
    "slv_nonposted_wr_data_word_received1",
    "mst_rd_data_word_received0",
    "mst_rd_data_word_received1",
];

/// Counter files whose sum is "data out of the chip".
const OUT_COUNTERS: [&str; 6] = [
    "slv_rd_data_word_sent0",
    "slv_rd_data_word_sent1",
    "mst_posted_wr_data_word_sent0",
    "mst_posted_wr_data_word_sent1",
    "mst_nonposted_wr_data_word_sent0",
    "mst_nonposted_wr_data_word_sent1",
];

/// Read and fold the counter directory. `None` if the directory (or every
/// file in it) is unreadable — i.e. tt-kmd < 2.x without the attribute group.
/// Individual missing files count as 0 so a partial set still reports.
pub fn read_counters(dir: &Path) -> Option<PcieCounterSnapshot> {
    if !dir.is_dir() {
        return None;
    }
    let sum = |names: &[&str]| -> u64 {
        names
            .iter()
            .filter_map(|n| {
                fs::read_to_string(dir.join(n))
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
            })
            .sum()
    };
    Some(PcieCounterSnapshot {
        words_in: sum(&IN_COUNTERS),
        words_out: sum(&OUT_COUNTERS),
    })
}

/// Turns successive snapshots into bytes/sec. The first sample primes the
/// tracker and yields `None`; a counter that goes backwards (device reset)
/// clamps its delta to 0 rather than underflowing.
#[derive(Debug, Default)]
pub struct PcieRateTracker {
    prev: Option<(PcieCounterSnapshot, Instant)>,
}

impl PcieRateTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sample(&mut self, snap: PcieCounterSnapshot, now: Instant) -> Option<PcieBandwidth> {
        let result = self.prev.and_then(|(prev, prev_t)| {
            let dt = now.duration_since(prev_t).as_secs_f64();
            if dt <= 0.0 {
                return None;
            }
            let d_in = snap.words_in.saturating_sub(prev.words_in);
            let d_out = snap.words_out.saturating_sub(prev.words_out);
            Some(PcieBandwidth {
                rx_bytes_per_sec: (d_in * BYTES_PER_WORD) as f64 / dt,
                tx_bytes_per_sec: (d_out * BYTES_PER_WORD) as f64 / dt,
            })
        });
        self.prev = Some((snap, now));
        result
    }
}

/// Human-readable rate with SI decimal units, ≤ 3 significant digits.
pub fn format_bandwidth(bytes_per_sec: f64) -> String {
    let b = bytes_per_sec.max(0.0);
    if b < 1000.0 {
        format!("{:.0} B/s", b)
    } else if b < 1_000_000.0 {
        format!("{:.1} kB/s", b / 1000.0)
    } else if b < 1_000_000_000.0 {
        format!("{:.0} MB/s", b / 1_000_000.0)
    } else {
        format!("{:.2} GB/s", b / 1_000_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant};

    fn fake_counters(dir: &std::path::Path, base: u64) {
        // The 12 files tt-kmd exposes; controller-1 files are usually ~0 on
        // single-controller cards but must still be summed.
        for (f, v) in [
            ("mst_nonposted_wr_data_word_sent0", base + 100),
            ("mst_nonposted_wr_data_word_sent1", 0),
            ("mst_posted_wr_data_word_sent0", base + 200),
            ("mst_posted_wr_data_word_sent1", 0),
            ("mst_rd_data_word_received0", base + 300),
            ("mst_rd_data_word_received1", 0),
            ("slv_nonposted_wr_data_word_received0", base + 400),
            ("slv_nonposted_wr_data_word_received1", 7),
            ("slv_posted_wr_data_word_received0", base + 500),
            ("slv_posted_wr_data_word_received1", 0),
            ("slv_rd_data_word_sent0", base + 600),
            ("slv_rd_data_word_sent1", 3),
        ] {
            fs::write(dir.join(f), v.to_string()).unwrap();
        }
    }

    #[test]
    fn read_counters_sums_directions() {
        let td = tempfile::tempdir().unwrap();
        fake_counters(td.path(), 1000);
        let snap = read_counters(td.path()).unwrap();
        // in  = slv posted+nonposted wr received + mst rd received
        //     = (1400+7) + 1500 + 1300 = 4207
        assert_eq!(snap.words_in, 4207);
        // out = slv rd sent + mst posted+nonposted wr sent
        //     = (1600+3) + 1200 + 1100 = 3903
        assert_eq!(snap.words_out, 3903);
    }

    #[test]
    fn read_counters_missing_dir_is_none() {
        assert!(read_counters(std::path::Path::new("/nonexistent-xyz")).is_none());
    }

    #[test]
    fn rate_tracker_computes_bytes_per_sec() {
        let mut tracker = PcieRateTracker::new();
        let t0 = Instant::now();
        // First sample only primes the tracker.
        assert!(tracker
            .sample(PcieCounterSnapshot { words_in: 1000, words_out: 500 }, t0)
            .is_none());
        // 2 s later: +2000 words in, +1000 words out → ×4 bytes / 2 s.
        let bw = tracker
            .sample(
                PcieCounterSnapshot { words_in: 3000, words_out: 1500 },
                t0 + Duration::from_secs(2),
            )
            .unwrap();
        assert!((bw.rx_bytes_per_sec - 4000.0).abs() < 1e-6);
        assert!((bw.tx_bytes_per_sec - 2000.0).abs() < 1e-6);
    }

    #[test]
    fn rate_tracker_counter_reset_yields_zero_not_negative() {
        let mut tracker = PcieRateTracker::new();
        let t0 = Instant::now();
        tracker.sample(PcieCounterSnapshot { words_in: 5000, words_out: 5000 }, t0);
        // Counters went backwards (device reset) → clamp to 0, don't underflow.
        let bw = tracker
            .sample(
                PcieCounterSnapshot { words_in: 10, words_out: 10 },
                t0 + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(bw.rx_bytes_per_sec, 0.0);
        assert_eq!(bw.tx_bytes_per_sec, 0.0);
    }

    #[test]
    fn format_bandwidth_picks_sane_units() {
        assert_eq!(format_bandwidth(0.0), "0 B/s");
        assert_eq!(format_bandwidth(950.0), "950 B/s");
        assert_eq!(format_bandwidth(12_300.0), "12.3 kB/s");
        assert_eq!(format_bandwidth(340_000_000.0), "340 MB/s");
        assert_eq!(format_bandwidth(1_230_000_000.0), "1.23 GB/s");
    }
}
