// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! PCIe data-word counters from tt-kmd's sysfs class attributes.
//!
//! tt-kmd exposes `/sys/class/tenstorrent/tenstorrent!N/pcie_perf_counters/`
//! — 12 world-readable, monotonically increasing 32-bit counters. They are the
//! NoC interface unit (NIU) counters of the chip's PCIe tile, the same
//! registers `NIU_*` in tt-isa-documentation's `BlackholeA0/NoC/Counters.md`.
//!
//! **Unit.** One counter "word" is one NoC data flit: 512 bits, **64 bytes**.
//! It is not a 4-byte PCIe word. Measured on a Blackhole p300c (2026-10-29): a
//! 268,378,112-byte host→device copy moved `slv_rd_data_word_sent0` by
//! 4,193,421 words. 268,378,112 / 64 is 4,193,408, so the measured count is
//! within 0.01% of 1/64 word per byte.
//!
//! **Direction.** The names are from the NoC side of the PCIe tile, so
//! "slave" means a NoC request was aimed at the PCIe tile (the chip reaching
//! out to host memory), and "master" means the PCIe tile issued a NoC request
//! (the host reaching into the chip).
//!
//! * `slv_rd_data_word_sent*` — the chip read host memory: data **into** the
//!   chip. (Measured: this is what a ttnn host→device copy moves.)
//! * `slv_{posted,nonposted}_wr_data_word_received*` — the chip wrote host
//!   memory: data **out of** the chip. (Measured: this is what a device→host
//!   read-back moves.)
//! * `mst_{posted,nonposted}_wr_data_word_sent*` — the host wrote into the
//!   chip through a BAR/TLB window: data **into** the chip. *Inferred, not
//!   measured.* ttnn's bulk path never used it, so these counters stayed near
//!   zero in the calibration.
//! * `mst_rd_data_word_received*` — the host read from the chip through a
//!   window: data **out of** the chip. *Inferred, not measured.*
//!
//! Each name has a `0` and `1` suffix (the counter for NoC0 and NoC1); both are
//! summed. Reading these files is passive — no device open, no ARC message.
//!
//! **Wrap.** The counters are 32 bits, which is about 275 GB of traffic. The
//! difference is taken per counter with wrapping arithmetic, so a wrap between
//! two samples is a forward step. A backwards step (device reset zeroes the
//! counters) looks like a nearly full-range forward step, so any step over half
//! the range is treated as a reset and reported as zero. At one sample per
//! second a real step is far smaller than that. More than one wrap between two
//! samples cannot be detected.
//!
//! **Unreadable counters.** Each counter in a snapshot is `None` when its file
//! could not be read. A counter that is `None` on either side of a step adds
//! nothing for that step, and the next step starts from its first reading
//! again. Reading a missing file as 0 made its return a step of its whole
//! value, up to 2^31 words (137 GB) in one sample.

use std::fs;
use std::path::Path;
use std::time::Instant;

/// Bytes per counter word: one 512-bit NoC data flit.
const BYTES_PER_WORD: u64 = 64;

/// Counter files whose sum is "data into the chip".
const IN_COUNTERS: [&str; 6] = [
    "slv_rd_data_word_sent0",
    "slv_rd_data_word_sent1",
    "mst_posted_wr_data_word_sent0",
    "mst_posted_wr_data_word_sent1",
    "mst_nonposted_wr_data_word_sent0",
    "mst_nonposted_wr_data_word_sent1",
];

/// Counter files whose sum is "data out of the chip".
const OUT_COUNTERS: [&str; 6] = [
    "slv_posted_wr_data_word_received0",
    "slv_posted_wr_data_word_received1",
    "slv_nonposted_wr_data_word_received0",
    "slv_nonposted_wr_data_word_received1",
    "mst_rd_data_word_received0",
    "mst_rd_data_word_received1",
];

/// Raw cumulative counter values, one slot per file in [`IN_COUNTERS`] and
/// [`OUT_COUNTERS`], `None` where the file could not be read. They stay
/// separate because the 32-bit wrap has to be handled per counter; a sum of
/// wrapped counters can go down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcieCounterSnapshot {
    /// Counters for data that entered the chip, in [`IN_COUNTERS`] order.
    in_words: [Option<u32>; 6],
    /// Counters for data that left the chip, in [`OUT_COUNTERS`] order.
    out_words: [Option<u32>; 6],
}

impl PcieCounterSnapshot {
    /// Words that entered the chip, summed over the readable counters (not
    /// wrap-aware).
    pub fn words_in(&self) -> u64 {
        self.in_words.iter().flatten().map(|&w| u64::from(w)).sum()
    }

    /// Words that left the chip, summed over the readable counters (not
    /// wrap-aware).
    pub fn words_out(&self) -> u64 {
        self.out_words.iter().flatten().map(|&w| u64::from(w)).sum()
    }
}

/// Derived link bandwidth between two snapshots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PcieBandwidth {
    /// Bytes/second flowing INTO the chip.
    pub rx_bytes_per_sec: f64,
    /// Bytes/second flowing OUT of the chip.
    pub tx_bytes_per_sec: f64,
}

/// Read each named counter, reporting how many actually parsed. A missing or
/// unparsable file is `None`.
///
/// Parsed as `u32`: the driver prints these with `%u`, so a larger value is a
/// corrupted read and counts as unreadable. Truncating it would make up a
/// value.
fn read_each(dir: &Path, names: &[&str; 6]) -> ([Option<u32>; 6], usize) {
    let mut values = [None; 6];
    for (slot, name) in values.iter_mut().zip(names) {
        *slot = fs::read_to_string(dir.join(name))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
    }
    let parsed = values.iter().flatten().count();
    (values, parsed)
}

/// Read the counter directory.
///
/// `None` when the directory is absent (tt-kmd < 2.7, no attribute group) **or
/// when not a single counter file in it could be read** — an unreadable set is
/// "not measured", and reporting it as a snapshot made the UI render a confident
/// `▼0 B/s ▲0 B/s` for a link nobody had actually sampled. Individual missing
/// files are `None` in the snapshot, so a partial set (a kernel that grew a
/// counter, a permissions oddity on one file) still reports the counters it
/// could read.
pub fn read_counters(dir: &Path) -> Option<PcieCounterSnapshot> {
    if !dir.is_dir() {
        return None;
    }
    let (in_words, in_parsed) = read_each(dir, &IN_COUNTERS);
    let (out_words, out_parsed) = read_each(dir, &OUT_COUNTERS);
    if in_parsed + out_parsed == 0 {
        log::debug!(
            "pcie_perf_counters at {} exists but no counter file was readable",
            dir.display()
        );
        return None;
    }
    Some(PcieCounterSnapshot {
        in_words,
        out_words,
    })
}

/// Forward step of one 32-bit counter between two reads.
///
/// Wrapping subtraction turns a wrap into the right forward step. A step over
/// half the range is a counter that went backwards (device reset), not 2 GWords
/// of traffic in one sample, so it counts as 0.
fn forward_delta(now: u32, prev: u32) -> u64 {
    let d = now.wrapping_sub(prev);
    if d > u32::MAX / 2 {
        0
    } else {
        u64::from(d)
    }
}

/// Sum of the counters' forward steps. A counter unreadable on either side
/// adds nothing: with no value to step from (or to), its step is unknown.
/// Since the tracker keeps the newer snapshot, a counter that comes back is
/// primed by its first reading and counts again from the step after.
fn total_delta(now: &[Option<u32>; 6], prev: &[Option<u32>; 6]) -> u64 {
    now.iter()
        .zip(prev)
        .map(|(n, p)| match (n, p) {
            (Some(n), Some(p)) => forward_delta(*n, *p),
            _ => 0,
        })
        .sum()
}

/// Turns successive snapshots into bytes/sec. The first sample primes the
/// tracker and yields `None`. Each counter's step is wrap-aware and a counter
/// that went backwards (device reset) contributes 0; see [`forward_delta`].
/// A counter unreadable on either side of a step contributes 0; see
/// [`total_delta`].
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
            let d_in = total_delta(&snap.in_words, &prev.in_words);
            let d_out = total_delta(&snap.out_words, &prev.out_words);
            Some(PcieBandwidth {
                rx_bytes_per_sec: (d_in * BYTES_PER_WORD) as f64 / dt,
                tx_bytes_per_sec: (d_out * BYTES_PER_WORD) as f64 / dt,
            })
        });
        self.prev = Some((snap, now));
        result
    }
}

/// Human-readable rate with SI decimal units, capped at 9 characters total
/// (mantissa + space + unit) **unconditionally** — for every finite input,
/// not just realistic PCIe magnitudes. The Insights sidebar's PCIe row
/// renders this output for both directions on one line (e.g. "▼12.3 kB/s
/// ▲1.23 GB/s"), so an unbounded mantissa here directly overflows that
/// row's fixed width — see `pcie_row_parts` in `ui::tui`, which relies on
/// this 9-char cap to keep its two-row layout within the sidebar's
/// 30-column budget.
///
/// **Input clamp.** `bytes_per_sec` is first clamped to `[0.0, 999.5e12]`
/// (≈999.5 TB/s) — chosen as the largest value whose TB/s mantissa still
/// rounds to `< 1000` (so the loop/fallback logic below is genuinely
/// bounded, not just bounded "in practice"). Non-finite input (`NaN`,
/// `+inf`, `-inf`) is treated as `0.0` instead of being clamped: `f64::clamp`
/// propagates `NaN` unchanged rather than pulling it into range, so a NaN
/// counter read would otherwise flow straight through to `format!` and
/// render literally as `"NaN B/s"` — technically within the 9-char budget,
/// but numerically nonsensical. This can happen on a corrupted tt-kmd
/// counter read (bit-flipped word count, a subtraction over/underflow
/// upstream), which is not reachable from a healthy PCIe link but *is*
/// reachable from bad hardware/driver state — exactly the case where the
/// row must render *something* sane rather than something wide, garbled,
/// or `NaN`. Post-clamp, values that would have been enormous (or
/// non-finite) render identically to a saturated real link: `"999 TB/s"` or
/// `"1000 TB/s"` (whichever the rounding tier below picks — both fit).
///
/// **Unit + precision selection**, unchanged in spirit from before the
/// clamp: picks the largest unit (B/s → kB/s → MB/s → GB/s → TB/s) whose
/// mantissa is < 1000, then formats that mantissa at the highest decimal
/// precision (2, then 1, then 0) that still fits a 4-character budget
/// *after rounding* — checked against the actual formatted string length,
/// not predicted from the pre-rounding magnitude. That distinction matters:
/// a mantissa like 99.95 formatted to 1 decimal rounds UP to "100.0" (5
/// chars — one over budget) even though 99.95 itself looks like it should
/// fit a naive "< 100 → 1 decimal" rule. Falling back to 0 decimals here
/// yields "100" instead: cosmetically coarser right at that boundary, but
/// never wider than budget. Concretely, `999_990.0` B/s (999.99 kB/s) lands
/// on this fallback and renders as "1000 kB/s" (9 chars) rather than the
/// unbounded-precision "1000.0 kB/s" (11 chars, over budget) a fixed
/// `{:.1}` would have produced.
///
/// TB/s exists so the unit-selection loop has somewhere to land for large
/// inputs instead of leaving the GB/s mantissa to grow without bound —
/// combined with the input clamp above, this is what makes the "mantissa
/// < 1000 whenever a non-B/s unit is chosen" invariant hold for *every*
/// finite input, not merely realistic PCIe speeds (nothing on a real link
/// approaches even 100 GB/s; Gen5 x16 tops out around 63 GB/s).
pub fn format_bandwidth(bytes_per_sec: f64) -> String {
    // Non-finite inputs (NaN, ±inf) bypass the clamp entirely — see the
    // "Input clamp" section above for why `.clamp()` alone isn't safe here.
    let b = if bytes_per_sec.is_finite() {
        bytes_per_sec.clamp(0.0, 999.5e12)
    } else {
        0.0
    };

    const UNITS: [&str; 5] = ["B/s", "kB/s", "MB/s", "GB/s", "TB/s"];
    let mut mantissa = b;
    let mut unit_idx = 0;
    while mantissa >= 1000.0 && unit_idx < UNITS.len() - 1 {
        mantissa /= 1000.0;
        unit_idx += 1;
    }
    let unit = UNITS[unit_idx];

    if unit_idx == 0 {
        // Whole bytes/sec: never needs a decimal point, and the unit-
        // selection loop above guarantees mantissa < 1000 going in (though
        // rounding at {:.0} can still tip e.g. 999.6 up to "1000" — still
        // only 4 digits, well inside budget for the 3-char "B/s" suffix).
        return format!("{:.0} {}", mantissa, unit);
    }

    // Every unit past B/s shares a 4-character suffix ("kB/s".."TB/s"), so
    // "<mantissa> <unit>" totals 9 chars only if the mantissa itself is
    // <= 4 chars (a leading space separates it from the unit).
    const MANTISSA_BUDGET: usize = 4;
    for &decimals in &[2usize, 1, 0] {
        let s = format!("{:.*}", decimals, mantissa);
        if s.len() <= MANTISSA_BUDGET {
            return format!("{} {}", s, unit);
        }
    }
    // Genuinely unreachable now: the input clamp caps `b` at 999.5e12, so
    // the unit-selection loop can only ever hand this branch a `mantissa`
    // strictly less than 1000 (at TB/s, the top and final tier) — 0
    // decimals of a value < 1000 is always <= 4 chars. Before the clamp
    // was added, this branch WAS reachable (e.g. `format_bandwidth(1e30)`
    // fell through to here with a many-digit mantissa and produced a
    // multi-hundred-character string) — kept as a safety net rather than
    // an `unreachable!()`, since "defensively correct but slightly verbose"
    // beats "provably correct but panics if the proof is ever wrong."
    format!("{:.0} {}", mantissa, unit)
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
        // in  = slv rd sent (1600+3) + mst posted/nonposted wr sent (1200 + 1100)
        //     = 3903
        assert_eq!(snap.words_in(), 3903);
        // out = slv posted/nonposted wr received (1500 + 1400+7) + mst rd received 1300
        //     = 4207
        assert_eq!(snap.words_out(), 4207);
    }

    #[test]
    fn read_counters_missing_dir_is_none() {
        assert!(read_counters(std::path::Path::new("/nonexistent-xyz")).is_none());
    }

    /// A directory that exists but yields no readable counter is "not measured",
    /// not "zero traffic".
    ///
    /// Regression: the early return only covered `!dir.is_dir()`, and
    /// `filter_map(..).sum()` turned a fully unreadable set into a valid-looking
    /// `words_in: 0, words_out: 0` snapshot — which the rate tracker then
    /// differentiated into a confident `▼0 B/s ▲0 B/s` for a link that had never
    /// been sampled.
    #[test]
    fn read_counters_all_unreadable_is_none() {
        let td = tempfile::tempdir().unwrap();
        // Directory present, but none of the 12 expected files exist.
        assert!(read_counters(td.path()).is_none());

        // A file that exists but holds garbage doesn't count as a reading either.
        fs::write(td.path().join("slv_rd_data_word_sent0"), "not-a-number").unwrap();
        assert!(read_counters(td.path()).is_none());
    }

    /// A partial set still reports: the readable files are summed and the
    /// missing ones count as 0.
    #[test]
    fn read_counters_partial_set_reports_partial_sum() {
        let td = tempfile::tempdir().unwrap();
        fs::write(td.path().join("mst_rd_data_word_received0"), "1200").unwrap();
        fs::write(td.path().join("slv_rd_data_word_sent1"), "34").unwrap();

        let snap = read_counters(td.path()).expect("one readable counter is enough");
        // mst rd received is data out of the chip; slv rd sent is data in.
        assert_eq!(snap.words_out(), 1200);
        assert_eq!(snap.words_in(), 34);
    }

    /// All twelve tt-kmd files, zero unless `set` overrides them.
    fn write_files(dir: &std::path::Path, set: &[(&str, u64)]) {
        for base in [
            "mst_nonposted_wr_data_word_sent",
            "mst_posted_wr_data_word_sent",
            "mst_rd_data_word_received",
            "slv_nonposted_wr_data_word_received",
            "slv_posted_wr_data_word_received",
            "slv_rd_data_word_sent",
        ] {
            for ctrl in ["0", "1"] {
                fs::write(dir.join(format!("{base}{ctrl}")), "0").unwrap();
            }
        }
        for (f, v) in set {
            fs::write(dir.join(f), v.to_string()).unwrap();
        }
    }

    /// Bandwidth measured the way the sysfs backend does it: read the files,
    /// feed both snapshots to the tracker, one second apart.
    fn bandwidth_between(before: &[(&str, u64)], after: &[(&str, u64)]) -> PcieBandwidth {
        let td = tempfile::tempdir().unwrap();
        let mut tracker = PcieRateTracker::new();
        let t0 = Instant::now();
        write_files(td.path(), before);
        assert!(tracker
            .sample(read_counters(td.path()).unwrap(), t0)
            .is_none());
        write_files(td.path(), after);
        tracker
            .sample(
                read_counters(td.path()).unwrap(),
                t0 + Duration::from_secs(1),
            )
            .unwrap()
    }

    /// Calibrated on real hardware (2026-10-29, Blackhole p300c, tt-kmd 2.9):
    /// a 268,378,112-byte host→device copy through ttnn moved
    /// `slv_rd_data_word_sent0` by 4,193,421. That is 1/64 word per byte, so a
    /// counter word is one 64-byte NoC flit, not a 4-byte PCIe word.
    #[test]
    fn counter_word_is_one_64_byte_flit() {
        let bw = bandwidth_between(
            &[("slv_rd_data_word_sent0", 1_000)],
            &[("slv_rd_data_word_sent0", 1_000 + 4_193_421)],
        );
        let payload = 268_378_112.0;
        assert!(
            (bw.rx_bytes_per_sec - payload).abs() / payload < 0.001,
            "4,193,421 words must read as ~268 MB, got {} B",
            bw.rx_bytes_per_sec
        );
    }

    /// Same calibration run: the chip pulling host memory raises
    /// `slv_rd_data_word_sent`, and that is data going INTO the chip. The
    /// `slv_*` names are NoC-side: "slave" means a NoC request targeted the
    /// PCIe tile.
    #[test]
    fn host_data_pulled_by_the_chip_is_into_the_chip() {
        let bw = bandwidth_between(&[], &[("slv_rd_data_word_sent0", 1_000)]);
        assert_eq!(bw.rx_bytes_per_sec, 64_000.0);
        assert_eq!(bw.tx_bytes_per_sec, 0.0);
    }

    /// Calibration, device→host: a 256 MiB read-back moved
    /// `slv_nonposted_wr_data_word_received0`. Chip writes to host memory are
    /// data going OUT of the chip.
    #[test]
    fn chip_writes_to_host_memory_are_out_of_the_chip() {
        for file in [
            "slv_nonposted_wr_data_word_received0",
            "slv_posted_wr_data_word_received1",
        ] {
            let bw = bandwidth_between(&[], &[(file, 1_000)]);
            assert_eq!(bw.tx_bytes_per_sec, 64_000.0, "{file}");
            assert_eq!(bw.rx_bytes_per_sec, 0.0, "{file}");
        }
    }

    /// NOT measured: ttnn never took the host-initiated (BAR/MMIO) path, so the
    /// `mst_*` counters stayed near zero in the calibration. Their direction
    /// follows from the NoC-side naming (the PCIe tile is the NoC master for
    /// host-initiated accesses): host writes → `mst_*_wr_sent` → into the chip,
    /// host reads → `mst_rd_received` → out of the chip. If a later calibration
    /// through an MMIO path disagrees, this test is the one to change.
    #[test]
    fn host_initiated_access_direction_is_inferred_from_noc_side_naming() {
        let w = bandwidth_between(&[], &[("mst_posted_wr_data_word_sent0", 1_000)]);
        assert_eq!(w.rx_bytes_per_sec, 64_000.0);
        let w = bandwidth_between(&[], &[("mst_nonposted_wr_data_word_sent0", 1_000)]);
        assert_eq!(w.rx_bytes_per_sec, 64_000.0);
        let r = bandwidth_between(&[], &[("mst_rd_data_word_received0", 1_000)]);
        assert_eq!(r.tx_bytes_per_sec, 64_000.0);
    }

    /// The counters are 32-bit and wrap (about 275 GB of traffic at 64 B/word).
    /// A wrap between two samples is a forward step of the remaining words.
    ///
    /// Regression: files were parsed as u64 and summed, so one counter wrapping
    /// made the sum fall and the tracker clamped that sample to zero.
    #[test]
    fn a_counter_wrapping_between_samples_is_a_forward_step() {
        let bw = bandwidth_between(
            &[("slv_rd_data_word_sent0", 0xFFFF_FFF0)],
            &[("slv_rd_data_word_sent0", 0x10)],
        );
        // 0x10 words to reach the wrap + 0x10 after it = 32 words.
        assert_eq!(bw.rx_bytes_per_sec, 32.0 * 64.0);
    }

    /// The wrap must be handled per counter. Summing first hides it: here one
    /// counter wraps while another grows, and the sum goes down.
    #[test]
    fn wrap_is_per_counter_not_per_sum() {
        let bw = bandwidth_between(
            &[
                ("slv_rd_data_word_sent0", 0xFFFF_FF00),
                ("mst_posted_wr_data_word_sent0", 100),
            ],
            &[
                ("slv_rd_data_word_sent0", 0x100),
                ("mst_posted_wr_data_word_sent0", 150),
            ],
        );
        // 0x200 words on the wrapping counter + 50 on the other.
        assert_eq!(bw.rx_bytes_per_sec, (0x200 + 50) as f64 * 64.0);
    }

    /// A reset goes backwards by a small amount, which is also what a wrap of
    /// almost the full range looks like. The two are told apart by size: a
    /// real forward step in one sample is never more than half the counter
    /// range (2^31 words is 137 GB), so a bigger apparent step is a reset.
    #[test]
    fn a_reset_is_not_mistaken_for_a_wrap() {
        let bw = bandwidth_between(
            &[("slv_rd_data_word_sent0", 5_000)],
            &[("slv_rd_data_word_sent0", 10)],
        );
        assert_eq!(bw.rx_bytes_per_sec, 0.0);
        assert_eq!(bw.tx_bytes_per_sec, 0.0);
    }

    /// A counter file that cannot be read for a sample, then comes back.
    ///
    /// Regression: a missing file read as 0, so the step out of the gap was
    /// the counter's whole value. Below 2^31 words that passes the reset
    /// check, so a counter at a billion words came back as a 64 GB step in
    /// one second, spiking every PCIe display, the Die 3D clouds and its
    /// burst trigger.
    #[test]
    fn a_counter_file_that_drops_out_and_returns_adds_no_spike() {
        let td = tempfile::tempdir().unwrap();
        let mut tracker = PcieRateTracker::new();
        let t0 = Instant::now();
        let at = |k: u64| t0 + Duration::from_secs(k);
        // slv_rd_data_word_sent0 sits at a billion words; sent1 moves 10
        // words a second throughout and must be reported every time.
        let rates: Vec<Option<PcieBandwidth>> = [
            Some(1_000_000_000u64),
            None,
            Some(1_000_000_000 + 1_000),
            Some(1_000_000_000 + 3_000),
        ]
        .into_iter()
        .enumerate()
        .map(|(k, sent0)| {
            let mut set = vec![("slv_rd_data_word_sent1", 10 * k as u64)];
            if let Some(v) = sent0 {
                set.push(("slv_rd_data_word_sent0", v));
            }
            write_files(td.path(), &set);
            if sent0.is_none() {
                fs::remove_file(td.path().join("slv_rd_data_word_sent0")).unwrap();
            }
            tracker.sample(read_counters(td.path()).unwrap(), at(k as u64))
        })
        .collect();
        assert!(rates[0].is_none(), "the first sample primes");
        // The gap and the sample after it: only sent1's 10 words a second.
        for k in [1, 2] {
            assert_eq!(rates[k].unwrap().rx_bytes_per_sec, 640.0, "sample {k}");
        }
        // Re-primed: sent0's 2000 words count again, with sent1's 10.
        assert_eq!(rates[3].unwrap().rx_bytes_per_sec, 2_010.0 * 64.0);
    }

    /// A set of which only some counters can be read reports those, and a
    /// counter that becomes readable later starts from its first reading,
    /// not from 0.
    #[test]
    fn a_partial_set_still_reports_the_readable_counters() {
        let td = tempfile::tempdir().unwrap();
        let mut tracker = PcieRateTracker::new();
        let t0 = Instant::now();
        fs::write(td.path().join("slv_rd_data_word_sent0"), "5000").unwrap();
        let first = read_counters(td.path()).unwrap();
        assert_eq!(first.words_in(), 5000);
        assert!(tracker.sample(first, t0).is_none());
        // One second on: sent0 moved 100 words, and an out counter that was
        // missing now reads 1,500,000,000.
        fs::write(td.path().join("slv_rd_data_word_sent0"), "5100").unwrap();
        fs::write(
            td.path().join("slv_posted_wr_data_word_received0"),
            "1500000000",
        )
        .unwrap();
        let bw = tracker
            .sample(
                read_counters(td.path()).unwrap(),
                t0 + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(bw.rx_bytes_per_sec, 6_400.0);
        assert_eq!(bw.tx_bytes_per_sec, 0.0, "a new counter is primed: no step");
    }

    #[test]
    fn format_bandwidth_picks_sane_units() {
        assert_eq!(format_bandwidth(0.0), "0 B/s");
        assert_eq!(format_bandwidth(950.0), "950 B/s");
        assert_eq!(format_bandwidth(12_300.0), "12.3 kB/s");
        assert_eq!(format_bandwidth(340_000_000.0), "340 MB/s");
        assert_eq!(format_bandwidth(1_230_000_000.0), "1.23 GB/s");
    }

    /// Pins the two rounding-boundary regressions the review flagged: a
    /// fixed `{:.1}`/`{:.2}` per unit (the pre-fix implementation) blew the
    /// 9-char budget exactly at these two magnitudes. Exact strings reflect
    /// what this implementation's fallback-decimals tiering actually
    /// produces (see `format_bandwidth`'s doc comment for why).
    #[test]
    fn format_bandwidth_new_tier_boundary_pins() {
        // >= 10 GB/s per direction is realistic on Gen4 x16 (theoretical
        // max ~31.5 GB/s) — the old fixed-{:.2} GB/s formatting grew to
        // "31.50 GB/s" (10 chars) as soon as the integer part hit 2 digits.
        assert_eq!(format_bandwidth(31_500_000_000.0), "31.5 GB/s");
        // kB/s→MB/s rounding boundary: the old fixed-{:.1} kB/s formatting
        // produced "1000.0 kB/s" (11 chars) here because 999.99 rounds UP
        // to 1000 at one decimal place, gaining a 4th mantissa digit.
        assert_eq!(format_bandwidth(999_990.0), "1000 kB/s");
    }

    /// Width-regression test: no magnitude may ever push `format_bandwidth`
    /// past the 9-character budget the Insights sidebar's PCIe row depends
    /// on (see `pcie_row_parts` / `pcie_row_parts_worst_case_fits_budget`
    /// in `ui::tui`). Sweeps every unit-tier boundary (exponents 0..=11,
    /// i.e. up to ~1e14 B/s — far beyond any real PCIe link, deliberately,
    /// to prove the cap is unconditional) crossed with mantissas chosen to
    /// land right at rounding cliffs (X.94 stays put, X.95 rounds up and
    /// risks gaining a digit) plus X.99 (the closest-to-rollover case).
    ///
    /// This sweep alone does NOT prove the 9-char cap is unconditional —
    /// exponents only up to 11 (~1e14) stay inside a range the pre-clamp
    /// implementation already handled correctly by landing on the TB/s
    /// tier with a sub-1000 mantissa. `format_bandwidth_saturates_extreme_and_non_finite_input`
    /// below is what actually exercises the clamp (values the unit-loop
    /// alone can't bound: 1e16, 1e30, `f64::MAX`) and the NaN/±inf guard.
    #[test]
    fn format_bandwidth_output_never_exceeds_9_chars() {
        for exp in 0..=11 {
            for &m in &[1.0, 9.94, 9.95, 99.94, 99.95, 999.94, 999.95, 999.99] {
                let value = m * 10f64.powi(exp);
                let s = format_bandwidth(value);
                assert!(
                    s.chars().count() <= 9,
                    "format_bandwidth({value}) = {s:?} is {} chars, over the 9-char budget",
                    s.chars().count()
                );
                // The output is pure ASCII (digits, '.', space, unit
                // letters) — char count and unicode display width must
                // agree. If they ever diverge, a non-ASCII character crept
                // into a unit string and the sidebar's column math (which
                // uses unicode_width, not byte/char length) would be wrong.
                assert_eq!(
                    s.chars().count(),
                    unicode_width::UnicodeWidthStr::width(s.as_str()),
                    "format_bandwidth output must be pure ASCII: {s:?}"
                );
            }
        }
    }

    /// The residual the second re-review caught: before the input clamp was
    /// added, the unit-selection loop stopped at TB/s and left the mantissa
    /// unbounded for anything above ~1e15 B/s — `format_bandwidth(1e30)`
    /// fell through the decimal-fallback loop and hit its "unreachable"
    /// last-resort branch with a mantissa still in the hundreds of
    /// sextillions, producing a many-hundred-character string
    /// ("1000000000000000000000000000 TB/s"-shaped) that would have blown
    /// the sidebar row wide open. Unreachable from a healthy PCIe link, but
    /// reachable from a corrupted tt-kmd counter read (bit-flip, subtraction
    /// underflow upstream) — exactly the moment the UI must not misrender.
    ///
    /// Covers every non-realistic input class the review named: two
    /// astronomically large finite values (1e16, 1e30), the largest finite
    /// f64 (`f64::MAX`), both infinities, `NaN`, and a negative value (the
    /// pre-existing `.max(0.0)` behavior, still exercised now that it's
    /// folded into the finite branch of the clamp guard).
    #[test]
    fn format_bandwidth_saturates_extreme_and_non_finite_input() {
        // Every huge-but-finite input saturates at the same clamped
        // ceiling (999.5e12) and therefore renders identically — pinned
        // against the actual output rather than assumed, per the
        // coordinator's instruction ("pin what it actually produces").
        for &huge in &[1e16, 1e30, f64::MAX] {
            let s = format_bandwidth(huge);
            assert_eq!(
                s, "1000 TB/s",
                "format_bandwidth({huge:e}) should saturate to the clamped ceiling"
            );
        }
        // Non-finite and negative input must not panic and must render
        // deterministically rather than propagating NaN/inf into the
        // displayed string (`f64::clamp` alone would let NaN through
        // unchanged — see the doc comment on `format_bandwidth` for why the
        // `is_finite()` guard exists).
        for &bad in &[f64::INFINITY, f64::NEG_INFINITY, f64::NAN, -5.0_f64] {
            assert_eq!(
                format_bandwidth(bad),
                "0 B/s",
                "non-finite/negative input {bad:?} must render as a sane zero, not propagate"
            );
        }
        // Every case above must also respect the general width budget —
        // redundant with the exact pins, but keeps this test self-contained
        // as a width-regression guard even if the exact strings above ever
        // legitimately change.
        for &v in &[
            1e16,
            1e30,
            f64::MAX,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
            -5.0,
        ] {
            let s = format_bandwidth(v);
            assert!(
                s.chars().count() <= 9,
                "format_bandwidth({v:?}) = {s:?} is {} chars, over the 9-char budget",
                s.chars().count()
            );
        }
    }
}
