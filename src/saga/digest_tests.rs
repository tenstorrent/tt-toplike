//! Tests for `--saga-digest` (`digest.rs`). The log is the saga crate's
//! busy fixture, written into a fresh segment directory by the bus crate's
//! own writer, so every record gets a new sequence number and new times,
//! as a replay host's recording does.

use super::*;
use std::path::PathBuf;
use tt_toplike_bus::segment::read_segment;
use tt_toplike_bus::{Bus, BusConfig, Record};

/// The saga crate's busy fixture, as a path.
fn busy_path() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/tt-toplike-saga/tests/fixtures/busy.jsonl")
}

/// Every record of the busy fixture, in file order.
fn busy_records() -> Vec<Record> {
    let read = read_segment(&busy_path()).expect("busy.jsonl is committed");
    assert_eq!(read.bad_lines, 0, "busy.jsonl decodes");
    read.records
}

/// A segment directory holding `records`, republished through a bus with a
/// log directory: the bus numbers and times each record itself.
fn write_log(records: &[Record]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let bus = Bus::open(BusConfig {
        dir: Some(dir.path().to_path_buf()),
        ..BusConfig::default()
    })
    .unwrap();
    for r in records {
        bus.publish(&r.source, r.payload.clone());
    }
    // Dropping the bus flushes its writer and closes the segment.
    drop(bus);
    dir
}

/// What a saga with the command's config tells when fed `records` directly:
/// the digest, worlds born and beats written.
fn told(records: &[Record]) -> (u64, u64, u64) {
    let mut saga = Saga::new(digest_config(None));
    let (mut worlds, mut beats) = (0, 0);
    for r in records {
        saga.feed(r);
        for ev in saga.drain_events() {
            match ev {
                SagaEvent::WorldBorn { .. } => worlds += 1,
                SagaEvent::Beat { .. } => beats += 1,
                _ => {}
            }
        }
    }
    (saga.digest(), worlds, beats)
}

/// The config depends on the log alone: the replay label, real names, and
/// the season the header records, or the default 20 minutes at 10 Hz for a
/// log without one. A reader's own config file cannot move it.
#[test]
fn the_config_is_replay_real_names_and_the_recorded_season() {
    let c = digest_config(None);
    assert_eq!(c.label, Label::Replay);
    assert_eq!(c.names, NameMode::Real);
    assert_eq!(c.season_ticks, 12_000);
    let header = |tags: &[(&str, &str)]| tt_toplike_bus::Header {
        schema: 1,
        kind: tt_toplike_bus::HeaderKind::Live,
        tool_versions: tags
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    };
    let c = digest_config(Some(&header(&[("saga.season_ticks", "600")])));
    assert_eq!((c.label, c.season_ticks), (Label::Replay, 600));
    // No tag, or a tag that is not a whole number above zero: the default.
    for bad in [
        &[][..],
        &[("saga.season_ticks", "0")],
        &[("saga.season_ticks", "x")],
    ] {
        assert_eq!(digest_config(Some(&header(bad))).season_ticks, 12_000);
    }
}

/// A log the bus wrote afresh (new sequence numbers and times) gives the
/// digest, worlds and beats of the fixture's records fed straight to the
/// core. The sequence numbers do differ, so the check covers them.
#[test]
fn a_rewritten_log_gives_the_digest_of_its_records() {
    let records = busy_records();
    let dir = write_log(&records);
    let d = digest_log(dir.path()).unwrap();

    let written = Replay::open(dir.path())
        .unwrap()
        .records()
        .collect::<Vec<_>>();
    assert_eq!(written.len(), records.len(), "the bus added no record");
    assert!(
        written
            .iter()
            .zip(&records)
            .any(|(w, r)| w.seq != r.seq || w.t_mono_ns != r.t_mono_ns),
        "the writer renumbered the records"
    );

    let (digest, worlds, beats) = told(&records);
    assert_eq!(d.digest, digest, "0x{:016x} vs 0x{digest:016x}", d.digest);
    assert_eq!((d.worlds_born, d.beats_written), (worlds, beats));
    assert!(worlds >= 1 && beats > worlds, "the busy log tells a story");
    assert_eq!(d.records, records.len() as u64);
    assert_eq!((d.bad_lines, d.holes), (0, 0));
}

/// Two runs over one log print the same lines.
#[test]
fn the_command_prints_the_same_digest_twice() {
    let run = || {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = digest_command(&busy_path(), &mut out, &mut err);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        assert!(err.is_empty());
        String::from_utf8(out).unwrap()
    };
    let first = run();
    assert_eq!(first, run());
    let lines: Vec<&str> = first.lines().collect();
    assert_eq!(lines.len(), 2, "{first}");
    let hex = lines[0].strip_prefix("saga digest: 0x").expect(lines[0]);
    assert_eq!(hex.len(), 16, "{}", lines[0]);
    assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()), "{}", lines[0]);
    let (digest, worlds, beats) = told(&busy_records());
    assert_eq!(u64::from_str_radix(hex, 16).unwrap(), digest);
    assert!(
        lines[1].starts_with(&format!(
            "worlds born: {worlds}, beats written: {beats}, records: "
        )),
        "{}",
        lines[1]
    );
}

/// A log that cannot be opened exits 1 with a message on the error stream
/// and writes nothing to the output.
#[test]
fn a_missing_log_exits_one() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(digest_command(&missing, &mut out, &mut err), 1);
    assert!(out.is_empty());
    let e = String::from_utf8(err).unwrap();
    assert!(e.starts_with("saga-digest: cannot read "), "{e}");
}

/// An output that fails with a closed pipe is success (`| head`); any other
/// output failure exits 1 with its own message.
#[test]
fn output_failures_follow_bus_inspect() {
    struct Failing(io::ErrorKind);
    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(self.0, "failing"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut err = Vec::new();
    let code = digest_command(
        &busy_path(),
        &mut Failing(io::ErrorKind::BrokenPipe),
        &mut err,
    );
    assert_eq!(code, 0);
    assert!(err.is_empty());

    let code = digest_command(&busy_path(), &mut Failing(io::ErrorKind::Other), &mut err);
    assert_eq!(code, 1);
    let e = String::from_utf8(err).unwrap();
    assert!(e.starts_with("saga-digest: cannot write the digest"), "{e}");
}
