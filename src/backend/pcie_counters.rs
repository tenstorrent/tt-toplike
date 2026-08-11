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

/// Sum the readable counters in `names`, reporting how many actually parsed.
///
/// `saturating_add` rather than `Iterator::sum`: these are firmware-owned u64
/// registers, and a corrupted read summing past `u64::MAX` would panic in a
/// debug build rather than render a wrong number.
fn sum_counters(dir: &Path, names: &[&str]) -> (u64, usize) {
    let mut total = 0u64;
    let mut parsed = 0usize;
    for name in names {
        if let Some(v) = fs::read_to_string(dir.join(name))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
        {
            total = total.saturating_add(v);
            parsed += 1;
        }
    }
    (total, parsed)
}

/// Read and fold the counter directory.
///
/// `None` when the directory is absent (tt-kmd < 2.7, no attribute group) **or
/// when not a single counter file in it could be read** — an unreadable set is
/// "not measured", and reporting it as a snapshot made the UI render a confident
/// `▼0 B/s ▲0 B/s` for a link nobody had actually sampled. Individual missing
/// files still count as 0, so a partial set (a kernel that grew a counter, a
/// permissions oddity on one file) reports the partial sum rather than nothing.
pub fn read_counters(dir: &Path) -> Option<PcieCounterSnapshot> {
    if !dir.is_dir() {
        return None;
    }
    let (words_in, in_parsed) = sum_counters(dir, &IN_COUNTERS);
    let (words_out, out_parsed) = sum_counters(dir, &OUT_COUNTERS);
    if in_parsed + out_parsed == 0 {
        log::debug!(
            "pcie_perf_counters at {} exists but no counter file was readable",
            dir.display()
        );
        return None;
    }
    Some(PcieCounterSnapshot {
        words_in,
        words_out,
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
        assert_eq!(snap.words_in, 1200);
        assert_eq!(snap.words_out, 34);
    }

    #[test]
    fn rate_tracker_computes_bytes_per_sec() {
        let mut tracker = PcieRateTracker::new();
        let t0 = Instant::now();
        // First sample only primes the tracker.
        assert!(tracker
            .sample(
                PcieCounterSnapshot {
                    words_in: 1000,
                    words_out: 500
                },
                t0
            )
            .is_none());
        // 2 s later: +2000 words in, +1000 words out → ×4 bytes / 2 s.
        let bw = tracker
            .sample(
                PcieCounterSnapshot {
                    words_in: 3000,
                    words_out: 1500,
                },
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
        tracker.sample(
            PcieCounterSnapshot {
                words_in: 5000,
                words_out: 5000,
            },
            t0,
        );
        // Counters went backwards (device reset) → clamp to 0, don't underflow.
        let bw = tracker
            .sample(
                PcieCounterSnapshot {
                    words_in: 10,
                    words_out: 10,
                },
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
