//! `--bus-inspect DIR --saga-digest`: the saga a recorded log tells, as one
//! number.
//!
//! The command reads every record of a recorded log in order, feeds each to
//! a fresh [`Saga`], and prints the saga's [`Saga::digest`] with the number
//! of worlds born and chronicle beats written. The saga core promises that
//! the same ordered stream of records gives the same saga (spec 3.5): it
//! reads only each record's source and payload, never its sequence number
//! or its timestamps. So a log and a replay of that log recorded by another
//! host, which numbers and times every record afresh, print the same digest.
//! That makes the command a check of the determinism contract with the real
//! binary (see `docs/saga.md`).
//!
//! # The saga it runs
//!
//! The config is fixed, so the digest depends on the records alone and not
//! on the reader's own config file:
//!
//! * the label is [`Label::Replay`]: the chronicle's first line carries
//!   "(replay)", as it does for any recorded log played back. A saga that
//!   ran live under another label wrote another first line, so its digest
//!   differs from this one even on the same records;
//! * names are real ([`NameMode::Real`]);
//! * the season comes from the log's header (the tag
//!   `saga.season_ticks`, which the host that recorded it wrote from its
//!   own `[saga]` settings and `--interval`). A log recorded before the tag
//!   existed uses the default, 20 minutes at 10 Hz
//!   ([`super::config::default_season_ticks`]).
//!
//! # Output and exit status
//!
//! ```text
//! saga digest: 0x0b5598cead3a912f
//! worlds born: 1, beats written: 7, records: 6813 (skipped 0 bad lines), holes: 0
//! ```
//!
//! The exit status follows `--bus-inspect`
//! ([`tt_toplike_bus::inspect::inspect_command`]): 0 when the lines were
//! written, and also when the output is a closed pipe; 1 when the log cannot
//! be opened or the output fails another way, with a message on `err`.

use std::io::{self, Write};
use std::path::Path;

use tt_toplike_bus::replay::Replay;
use tt_toplike_bus::Header;
use tt_toplike_saga::chronicle::NameMode;
use tt_toplike_saga::{Label, Saga, SagaConfig, SagaEvent};

/// What the saga told about one log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Digest {
    /// [`Saga::digest`] after the last record.
    pub digest: u64,
    /// `WorldBorn` events raised.
    pub worlds_born: u64,
    /// `Beat` events raised: one per chronicle line written.
    pub beats_written: u64,
    /// Records read and fed to the saga.
    pub records: u64,
    /// Lines skipped because they did not decode.
    pub bad_lines: u64,
    /// Holes found in the log (see [`Replay::holes`]).
    pub holes: usize,
}

/// The saga config the command uses for a log whose first header is
/// `header` (`None` when it has no valid one). See "The saga it runs" in
/// the module comment.
pub fn digest_config(header: Option<&Header>) -> SagaConfig {
    let season_ticks = header
        .and_then(|h| super::config::season_from_tags(&h.tool_versions))
        .unwrap_or_else(super::config::default_season_ticks);
    SagaConfig {
        season_ticks,
        names: NameMode::Real,
        label: Label::Replay,
    }
}

/// Reads the log at `path` (a segment file or a directory of segments) and
/// runs the saga over it. Fails only when the log cannot be opened; bad
/// lines and holes are counted and skipped, as a replay skips them.
pub fn digest_log(path: &Path) -> io::Result<Digest> {
    let mut replay = Replay::open(path)?;
    let mut saga = Saga::new(digest_config(replay.header()));
    let (mut worlds_born, mut beats_written, mut records) = (0u64, 0u64, 0u64);
    for rec in replay.records() {
        saga.feed(&rec);
        records += 1;
        // Drain after every record: the core keeps a bounded buffer of
        // undrained events and would drop the oldest of a long backlog.
        for ev in saga.drain_events() {
            match ev {
                SagaEvent::WorldBorn { .. } => worlds_born += 1,
                SagaEvent::Beat { .. } => beats_written += 1,
                _ => {}
            }
        }
    }
    Ok(Digest {
        digest: saga.digest(),
        worlds_born,
        beats_written,
        records,
        bad_lines: replay.bad_lines(),
        holes: replay.hole_count(),
    })
}

/// Writes the two lines for `d` (see the module comment).
pub fn write_digest(d: &Digest, out: &mut impl Write) -> io::Result<()> {
    writeln!(out, "saga digest: 0x{:016x}", d.digest)?;
    writeln!(
        out,
        "worlds born: {}, beats written: {}, records: {} (skipped {} bad lines), holes: {}",
        d.worlds_born, d.beats_written, d.records, d.bad_lines, d.holes
    )
}

/// The `--saga-digest` command: digest the log at `path`, write the lines
/// to `out`, flush, and return the exit status (see the module comment).
/// A failure to write the error message to `err` is ignored, as
/// `--bus-inspect` ignores it, so `2>&1 | head` cannot end in a panic.
pub fn digest_command(path: &Path, out: &mut impl Write, err: &mut impl Write) -> i32 {
    let d = match digest_log(path) {
        Ok(d) => d,
        Err(e) => {
            let _ = writeln!(err, "saga-digest: cannot read {}: {e}", path.display());
            return 1;
        }
    };
    match write_digest(&d, out).and_then(|()| out.flush()) {
        Ok(()) => 0,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => 0,
        Err(e) => {
            let _ = writeln!(err, "saga-digest: cannot write the digest: {e}");
            1
        }
    }
}

#[cfg(test)]
#[path = "digest_tests.rs"]
mod digest_tests;
