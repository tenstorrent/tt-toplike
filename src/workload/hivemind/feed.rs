// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Coalesced live event feed.
//!
//! Live-test feedback on the raw `hive.events()` feed was that it floods with
//! dozens of near-identical lines — the worst offender being device-poll
//! open/close churn from `collectors::procfs`/`collectors::wrap` (a busy
//! `vLLM` process opening and closing the same `/dev/tenstorrent/N` fd over
//! and over shows up as two new lines per cycle, forever scrolling everything
//! else off screen).
//!
//! `FeedAgg` is a pure, terminal-free aggregator: it folds every ingested
//! event into a `(source, device, pattern)`-keyed row, tracking a running
//! `count` and a short rolling events/sec `rate` (via per-second buckets),
//! instead of storing every event's own line. This is a VIEW over the event
//! stream, not a replacement for it — `Hivemind`'s bounded ring
//! (`events()`/`dropped()`) is untouched; `FeedAgg` only evicts its own
//! (already-stale) coalesced rows under memory pressure, which is a different
//! thing from dropping raw events.

use super::event::{EventKind, Severity, SniffEvent, Source};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Number of 1-second rate/spark buckets tracked per row. A short window
/// (a few seconds) is enough to show "is this still happening right now"
/// without needing much memory per row.
const BUCKETS: usize = 6;

/// Cadence at which buckets roll.
const BUCKET_DUR: Duration = Duration::from_secs(1);

/// Cap on tracked rows. Beyond this, the least-recently-active row is
/// evicted — rows are coalesced aggregates over the live event stream, not
/// history, so dropping a stale one loses nothing the ring doesn't already
/// hold.
const MAX_ROWS: usize = 400;

/// Find the device index in the OPEN/CLOSE forms `collectors::procfs` and
/// `collectors::wrap` emit for `/dev/tenstorrent/N` fd events:
/// - `"<name> (pid P) opened /dev/tenstorrent/N"`
/// - `"<name> (pid P) closed /dev/tenstorrent/N"`
/// - `"pid P closed /dev/tenstorrent/N"` (source unknown at close time)
///
/// Returns `None` for any other text — this is a plain substring scan, not a
/// classifier, so it only fires on lines that actually say "opened"/"closed"
/// immediately followed by the device path.
fn device_open_close_target(text: &str) -> Option<u8> {
    for marker in ["opened /dev/tenstorrent/", "closed /dev/tenstorrent/"] {
        if let Some(idx) = text.find(marker) {
            let rest = &text[idx + marker.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse::<u8>() {
                return Some(n);
            }
        }
    }
    None
}

/// Replace every run of ASCII digits in `text` with a single `#`, trimming
/// the result. Folds pid/address/counter-varying lines (e.g. "load 3 shards"
/// / "load 42 shards") into one pattern.
fn generic_fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.trim().chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            out.push('#');
            while matches!(chars.peek(), Some(d) if d.is_ascii_digit()) {
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Build a stable coalescing pattern from an event, so repeated/near-
/// identical events fold into the same feed row.
///
/// Two rules, in order:
///
/// 1. **Device-poll fold**: if `text` matches either the OPEN or CLOSE form
///    of a `/dev/tenstorrent/N` fd event (see `device_open_close_target`),
///    both map to ONE canonical pattern, `"open+close /dev/tenstorrent/N"` —
///    keeping the device index, dropping the pid. A process's open and its
///    later matching close on the same device thus fold into a single row
///    instead of appearing (and disappearing) as two separate churny lines.
/// 2. **Generic fold**: otherwise, `generic_fold` replaces digit runs with
///    `#`.
///
/// `source` and `kind` are accepted — matching the aggregator's full row key
/// `(source, device, pattern)` in `FeedAgg` — but are not needed to build the
/// pattern text itself: the caller's key tuple already keeps a `Vllm`/D0 row
/// distinct from a `Vllm`/D1 row (different `device`) or from a `TtMetal` row
/// (different `source`), even when two sources' patterns happen to coincide.
/// `device` likewise isn't consulted directly — the fold only fires when the
/// *text* actually says "opened"/"closed" next to the device path, so an
/// unrelated event that merely happens to carry `device: Some(n)` isn't
/// mistaken for fd churn.
pub fn coalesce_key(_source: Source, _device: Option<u8>, _kind: EventKind, text: &str) -> String {
    if let Some(dev) = device_open_close_target(text) {
        return format!("open+close /dev/tenstorrent/{dev}");
    }
    generic_fold(text)
}

/// One coalesced row: an aggregate over every event that folded to the same
/// `(source, device, pattern)` key.
struct FeedRow {
    source: Source,
    device: Option<u8>,
    display: String,
    count: u64,
    max_severity: Severity,
    last: Instant,
    /// Per-second event counts, oldest first, rolled by `tick`.
    buckets: VecDeque<u32>,
    /// Events folded into this row since the last roll — becomes the next
    /// bucket once `tick` rolls.
    current_bucket: u32,
    /// Recomputed by `tick`: `sum(buckets) / BUCKETS`, events/sec.
    rate: f32,
}

/// A read-only snapshot of one coalesced row, returned by `FeedAgg::rows`.
pub struct FeedRowView {
    pub source: Source,
    pub device: Option<u8>,
    pub display: String,
    pub count: u64,
    pub max_severity: Severity,
    pub rate: f32,
    pub last: Instant,
    /// Recent per-second activity, oldest first, for a tiny feed-row
    /// sparkline. Length is always `BUCKETS` (zero-padded at the front until
    /// enough ticks have elapsed).
    pub spark: Vec<u8>,
}

/// Coalescing aggregator: folds a stream of `SniffEvent`s into count+rate
/// rows keyed by `(source, device, coalesce_key(...))`. See the module docs
/// and `coalesce_key` for the folding rules.
pub struct FeedAgg {
    rows: HashMap<(Source, Option<u8>, String), FeedRow>,
    last_roll: Option<Instant>,
}

impl FeedAgg {
    pub fn new() -> Self {
        Self {
            rows: HashMap::new(),
            last_roll: None,
        }
    }

    /// Fold one drained event into its row: upsert by `(source, device,
    /// pattern)`, bumping `count`, `last`, `max_severity`, and the current
    /// (not-yet-rolled) rate bucket.
    pub fn ingest(&mut self, ev: &SniffEvent, now: Instant) {
        let pattern = coalesce_key(ev.source, ev.device, ev.kind, &ev.text);
        let key = (ev.source, ev.device, pattern.clone());
        let row = self.rows.entry(key).or_insert_with(|| FeedRow {
            source: ev.source,
            device: ev.device,
            display: pattern,
            count: 0,
            max_severity: ev.severity,
            last: now,
            buckets: VecDeque::with_capacity(BUCKETS),
            current_bucket: 0,
            rate: 0.0,
        });
        row.count += 1;
        row.last = now;
        row.current_bucket = row.current_bucket.saturating_add(1);
        if ev.severity > row.max_severity {
            row.max_severity = ev.severity;
        }
        self.evict_if_over_cap();
    }

    /// Roll the per-second rate/spark buckets on a ~1s cadence. Safe to call
    /// as often as `poll()` runs — it's a no-op until at least `BUCKET_DUR`
    /// has elapsed since the last roll. If more than one bucket's worth of
    /// time has passed (e.g. the sniffer was paused), the intervening
    /// buckets are rolled as zeros — correct, since nothing happened during
    /// that gap — rather than skipped, so a stale row's rate genuinely
    /// decays back toward zero instead of freezing at its last value.
    pub fn tick(&mut self, now: Instant) {
        let elapsed_secs = match self.last_roll {
            None => 1,
            Some(lr) => {
                let secs =
                    now.saturating_duration_since(lr).as_secs_f32() / BUCKET_DUR.as_secs_f32();
                if secs < 1.0 {
                    return;
                }
                // Cap so a very long gap doesn't spin the roll loop forever;
                // beyond BUCKETS seconds every row's history is zeroed either
                // way, so extra iterations wouldn't change the outcome.
                (secs.floor() as u64).min(BUCKETS as u64 + 1)
            }
        };

        for row in self.rows.values_mut() {
            for i in 0..elapsed_secs {
                let count = if i == 0 { row.current_bucket } else { 0 };
                row.buckets.push_back(count);
                if row.buckets.len() > BUCKETS {
                    row.buckets.pop_front();
                }
            }
            row.current_bucket = 0;
            let sum: u32 = row.buckets.iter().sum();
            row.rate = sum as f32 / BUCKETS as f32;
        }
        self.last_roll = Some(now);
    }

    /// Rows for rendering, newest-first (`last` descending), optionally
    /// filtered to one `(source, device)` cell (`None` = every row, i.e.
    /// unified mode), and dropping any row whose `max_severity` is below
    /// `sev_floor`.
    pub fn rows(
        &self,
        cell: Option<(Source, Option<u8>)>,
        sev_floor: Severity,
    ) -> Vec<FeedRowView> {
        let mut matched: Vec<&FeedRow> = self
            .rows
            .values()
            .filter(|r| {
                r.max_severity >= sev_floor
                    && cell.map_or(true, |(s, d)| r.source == s && r.device == d)
            })
            .collect();
        matched.sort_by(|a, b| b.last.cmp(&a.last));
        matched
            .into_iter()
            .map(|r| FeedRowView {
                source: r.source,
                device: r.device,
                display: r.display.clone(),
                count: r.count,
                max_severity: r.max_severity,
                rate: r.rate,
                last: r.last,
                spark: {
                    let pad = BUCKETS.saturating_sub(r.buckets.len());
                    let mut s = vec![0u8; pad];
                    s.extend(r.buckets.iter().map(|&c| c.min(u8::MAX as u32) as u8));
                    s
                },
            })
            .collect()
    }

    /// Drop the least-recently-active row once we're over `MAX_ROWS`. Rows
    /// are coalesced aggregates, not history, so this is memory hygiene, not
    /// event loss — it never touches `Hivemind`'s own ring/drop counter.
    fn evict_if_over_cap(&mut self) {
        if self.rows.len() <= MAX_ROWS {
            return;
        }
        if let Some(oldest_key) = self
            .rows
            .iter()
            .min_by_key(|(_, r)| r.last)
            .map(|(k, _)| k.clone())
        {
            self.rows.remove(&oldest_key);
        }
    }
}

impl Default for FeedAgg {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::hivemind::event::{EventKind, Severity, Source};

    fn ev(source: Source, device: Option<u8>, severity: Severity, text: &str) -> SniffEvent {
        SniffEvent {
            ts: Instant::now(),
            source,
            device,
            severity,
            kind: EventKind::Fd,
            text: text.to_string(),
            origin: "test".into(),
        }
    }

    // ── coalesce_key ─────────────────────────────────────────────────────

    #[test]
    fn open_and_close_same_device_fold_to_same_pattern() {
        let open = coalesce_key(
            Source::Vllm,
            Some(2),
            EventKind::Fd,
            "python (pid 100) opened /dev/tenstorrent/2",
        );
        let close = coalesce_key(
            Source::Vllm,
            Some(2),
            EventKind::Fd,
            "python (pid 100) closed /dev/tenstorrent/2",
        );
        let close_unknown_pid = coalesce_key(
            Source::Unknown,
            Some(2),
            EventKind::Fd,
            "pid 999 closed /dev/tenstorrent/2",
        );
        assert_eq!(open, close);
        assert_eq!(open, close_unknown_pid);
        assert_eq!(open, "open+close /dev/tenstorrent/2");
    }

    #[test]
    fn different_devices_fold_to_different_patterns() {
        let d0 = coalesce_key(
            Source::Vllm,
            Some(0),
            EventKind::Fd,
            "p (pid 1) opened /dev/tenstorrent/0",
        );
        let d1 = coalesce_key(
            Source::Vllm,
            Some(1),
            EventKind::Fd,
            "p (pid 1) opened /dev/tenstorrent/1",
        );
        assert_ne!(d0, d1);
    }

    #[test]
    fn generic_pid_varying_lines_collapse_across_pids() {
        let a = coalesce_key(
            Source::TtSmi,
            None,
            EventKind::Log,
            "ERROR weight load 3 shards",
        );
        let b = coalesce_key(
            Source::TtSmi,
            None,
            EventKind::Log,
            "ERROR weight load 42 shards",
        );
        assert_eq!(a, b);
        assert_eq!(a, "ERROR weight load # shards");
    }

    // ── FeedAgg ──────────────────────────────────────────────────────────

    #[test]
    fn identical_events_coalesce_into_one_row_with_count_and_rate() {
        let mut agg = FeedAgg::new();
        let t0 = Instant::now();
        for _ in 0..5 {
            agg.ingest(&ev(Source::Vllm, Some(0), Severity::Info, "same line"), t0);
        }
        agg.tick(t0 + Duration::from_secs(1));

        let rows = agg.rows(None, Severity::Trace);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].count, 5);
        assert!(rows[0].rate > 0.0, "rate should be > 0 right after a tick");
    }

    #[test]
    fn different_device_makes_a_second_row() {
        let mut agg = FeedAgg::new();
        let t0 = Instant::now();
        agg.ingest(&ev(Source::Vllm, Some(0), Severity::Info, "same line"), t0);
        agg.ingest(&ev(Source::Vllm, Some(1), Severity::Info, "same line"), t0);

        let rows = agg.rows(None, Severity::Trace);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn rows_are_returned_newest_first() {
        let mut agg = FeedAgg::new();
        let t0 = Instant::now();
        agg.ingest(&ev(Source::Vllm, Some(0), Severity::Info, "older line"), t0);
        let t1 = t0 + Duration::from_secs(3);
        agg.ingest(
            &ev(Source::TtMetal, Some(0), Severity::Info, "newer line"),
            t1,
        );

        let rows = agg.rows(None, Severity::Trace);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].source,
            Source::TtMetal,
            "most-recently-active row first"
        );
        assert_eq!(rows[1].source, Source::Vllm);
    }

    #[test]
    fn cell_filter_only_returns_matching_source_and_device() {
        let mut agg = FeedAgg::new();
        let t0 = Instant::now();
        agg.ingest(&ev(Source::Vllm, Some(0), Severity::Info, "a"), t0);
        agg.ingest(&ev(Source::TtMetal, Some(1), Severity::Info, "b"), t0);

        let rows = agg.rows(Some((Source::Vllm, Some(0))), Severity::Trace);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, Source::Vllm);
        assert_eq!(rows[0].device, Some(0));
    }

    #[test]
    fn severity_floor_drops_rows_below_it() {
        let mut agg = FeedAgg::new();
        let t0 = Instant::now();
        agg.ingest(&ev(Source::Vllm, Some(0), Severity::Trace, "a"), t0);
        agg.ingest(&ev(Source::TtMetal, Some(0), Severity::Error, "b"), t0);

        let rows = agg.rows(None, Severity::Warn);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, Source::TtMetal);
    }

    #[test]
    fn row_cap_evicts_least_recently_active() {
        let mut agg = FeedAgg::new();
        let t0 = Instant::now();
        let total = MAX_ROWS + 5;

        // Need `total` strictly DISTINCT `(source, device, pattern)` keys so
        // the cap is actually exceeded and eviction fires. Two traps to
        // avoid here (both present in the original, broken version of this
        // test):
        //   1. `device` is a `u8` (only 256 distinct values) — varying it
        //      alone caps out at 256 keys, well under `MAX_ROWS` (400), so
        //      the eviction branch would never trigger.
        //   2. A numeric suffix in the TEXT doesn't help either: `generic_fold`
        //      (deliberately) replaces every digit run with a single `#`, so
        //      "pattern-7" and "pattern-42" both fold to the same
        //      "pattern-#" and collapse into one row.
        // A letter-run has no digits, so `generic_fold` leaves it untouched,
        // and it's unique per iteration by length — keeping `source`/`device`
        // fixed isolates the assertions to pattern-driven eviction only.
        let pattern_for = |i: usize| format!("distinct-pattern-{}", "a".repeat(i + 1));

        for i in 0..total {
            agg.ingest(
                &ev(Source::Vllm, Some(0), Severity::Info, &pattern_for(i)),
                t0 + Duration::from_millis(i as u64),
            );
        }

        let rows = agg.rows(None, Severity::Trace);
        assert_eq!(
            rows.len(),
            MAX_ROWS,
            "row count should be capped at exactly {MAX_ROWS} once the cap is exceeded, got {}",
            rows.len()
        );

        // Eviction always removes the least-recently-active (smallest
        // `last`) row, so only the most-recently-ingested `MAX_ROWS`
        // iterations should survive; every earlier iteration must be gone.
        // This is the assertion that would actually fail if eviction were
        // broken or absent (it would instead see all `total` rows present).
        let surviving: std::collections::HashSet<String> =
            rows.iter().map(|r| r.display.clone()).collect();
        let evicted_cutoff = total - MAX_ROWS;
        for i in 0..evicted_cutoff {
            let pattern = pattern_for(i);
            assert!(
                !surviving.contains(&pattern),
                "oldest row (iteration {i}, pattern {pattern:?}) should have been evicted"
            );
        }
        for i in evicted_cutoff..total {
            let pattern = pattern_for(i);
            assert!(
                surviving.contains(&pattern),
                "recently-ingested row (iteration {i}, pattern {pattern:?}) should still be present"
            );
        }
    }
}
