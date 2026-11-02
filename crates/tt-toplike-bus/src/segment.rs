//! Segment files: naming, writing, compressing and reading.
//!
//! The log on disk is a directory of segment files. Each holds a run of
//! records in sequence order and is named after the first one:
//! `bus-<first_seq>.jsonl`. The first line is a [`Header`] (schema version,
//! `live` or `replay`, tool versions). Every other line is one
//! [`Record::to_line`].
//!
//! When a segment is closed it is compressed to `bus-<first_seq>.jsonl.gz`.
//! The compressed copy is written to a `.tmp` name, renamed into place, and
//! only then is the plain file removed, so at every moment at least one of
//! the two names exists. A reader tries the plain name first and falls back
//! to the compressed one.
//!
//! The directory is created with mode 0700 and every segment file with mode
//! 0600 (Unix), because process command lines and model names end up in the
//! log.
//!
//! # Compression crate
//!
//! `flate2` with its `rust_backend` feature (the pure-Rust `miniz_oxide`
//! deflate). Reasons: it is already in this workspace's dependency tree
//! (through `procfs`), so it adds no new code to audit or build; it has no C
//! dependency, so the build stays plain `cargo`; gzip files open with `zcat`
//! and Python's `gzip` module, which matters for tensixinet and debugging.
//! JSON Lines compresses well under deflate, so the ratio is close to zstd's
//! for this data. `cargo audit` on 2026-11-01 reported no advisories for
//! `flate2`, `miniz_oxide`, `crc32fast` or `adler2`.
//!
//! # Damaged files
//!
//! A reader skips a line that does not decode (a corrupt line, a line cut
//! off by a crash, invalid UTF-8) and counts it in
//! [`SegmentRead::bad_lines`]. It keeps reading past it.

use crate::record::{Header, HeaderKind, Record, SCHEMA_VERSION};
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

/// Extension added to a closed, compressed segment.
pub const COMPRESSED_EXT: &str = "gz";

const PREFIX: &str = "bus-";
const PLAIN_EXT: &str = ".jsonl";

/// What a segment read returned.
#[derive(Debug, Default)]
pub struct SegmentRead {
    /// The header line, if the first line was a valid header.
    pub header: Option<Header>,
    /// Records in file order.
    pub records: Vec<Record>,
    /// Lines that did not decode and were skipped.
    pub bad_lines: u64,
}

/// Read a whole segment, plain or compressed (chosen by the file name).
pub fn read_segment(path: &Path) -> io::Result<SegmentRead> {
    read_segment_from(path, 0, usize::MAX)
}

/// Read only the header line of a segment, plain or compressed. `None` when
/// the first line is not a valid header (or the file is empty).
pub(crate) fn read_header(path: &Path) -> io::Result<Option<Header>> {
    let file = File::open(path)?;
    let compressed = path
        .extension()
        .is_some_and(|e| e == std::ffi::OsStr::new(COMPRESSED_EXT));
    let reader: Box<dyn Read> = if compressed {
        Box::new(MultiGzDecoder::new(file))
    } else {
        Box::new(file)
    };
    let mut line = Vec::new();
    // A damaged stream reads as "no header".
    if BufReader::new(reader).read_until(b'\n', &mut line).is_err() {
        return Ok(None);
    }
    Ok(std::str::from_utf8(&line)
        .ok()
        .and_then(|t| serde_json::from_str::<Header>(t.trim_end_matches(['\n', '\r'])).ok()))
}

/// Read the records of a segment whose `seq` is at least `from_seq`,
/// stopping after `max` of them. Bad lines before the stopping point are
/// skipped and counted.
pub fn read_segment_from(path: &Path, from_seq: u64, max: usize) -> io::Result<SegmentRead> {
    let file = File::open(path)?;
    let compressed = path
        .extension()
        .is_some_and(|e| e == std::ffi::OsStr::new(COMPRESSED_EXT));
    let reader: Box<dyn Read> = if compressed {
        Box::new(MultiGzDecoder::new(file))
    } else {
        Box::new(file)
    };
    let mut reader = BufReader::new(reader);
    let mut out = SegmentRead::default();
    let mut line = Vec::new();
    let mut first = true;
    while out.records.len() < max {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) => {}
            // A damaged compressed stream ends the read; what came before
            // it is still returned.
            Err(_) => {
                out.bad_lines += 1;
                break;
            }
        }
        let text = match std::str::from_utf8(&line) {
            Ok(t) => t.trim_end_matches(['\n', '\r']),
            Err(_) => {
                out.bad_lines += 1;
                first = false;
                continue;
            }
        };
        if first {
            first = false;
            if let Ok(h) = serde_json::from_str::<Header>(text) {
                out.header = Some(h);
                continue;
            }
        }
        if text.is_empty() {
            continue;
        }
        match Record::from_line(text) {
            Ok(r) if r.seq >= from_seq => out.records.push(r),
            Ok(_) => {}
            Err(_) => out.bad_lines += 1,
        }
    }
    Ok(out)
}

/// `bus-<first_seq>.jsonl`, plus `.gz` when compressed.
pub(crate) fn file_name(first_seq: u64, compressed: bool) -> String {
    if compressed {
        format!("{PREFIX}{first_seq}{PLAIN_EXT}.{COMPRESSED_EXT}")
    } else {
        format!("{PREFIX}{first_seq}{PLAIN_EXT}")
    }
}

/// The inverse of [`file_name`]: `(first_seq, compressed)`.
pub(crate) fn parse_name(name: &str) -> Option<(u64, bool)> {
    let rest = name.strip_prefix(PREFIX)?;
    let (num, compressed) = if let Some(n) = rest.strip_suffix(&format!(".{COMPRESSED_EXT}")) {
        (n.strip_suffix(PLAIN_EXT)?, true)
    } else {
        (rest.strip_suffix(PLAIN_EXT)?, false)
    };
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((num.parse().ok()?, compressed))
}

/// The tool versions recorded in a header: this crate's name and version.
pub(crate) fn tool_versions() -> BTreeMap<String, String> {
    let mut v = BTreeMap::new();
    v.insert(
        env!("CARGO_PKG_NAME").to_string(),
        env!("CARGO_PKG_VERSION").to_string(),
    );
    v
}

/// The header line written at the top of every segment. `kind` is `Live`
/// for a normal run and `Replay` for a bus that republishes a recorded log.
#[cfg(test)]
pub(crate) fn header_line(kind: HeaderKind) -> String {
    header_line_tagged(kind, &BTreeMap::new())
}

/// The header with `tags` added to `tool_versions`. A tag never replaces an
/// entry [`tool_versions`] already has.
pub(crate) fn header(kind: HeaderKind, tags: &BTreeMap<String, String>) -> Header {
    let mut tools = tool_versions();
    for (k, v) in tags {
        tools.entry(k.clone()).or_insert_with(|| v.clone());
    }
    Header {
        schema: SCHEMA_VERSION,
        kind,
        tool_versions: tools,
    }
}

/// [`header_line`] with `tags` (see [`header`]).
pub(crate) fn header_line_tagged(kind: HeaderKind, tags: &BTreeMap<String, String>) -> String {
    serde_json::to_string(&header(kind, tags)).expect("header encodes as JSON")
}

/// Create `dir` (and missing parents) with mode 0700. A directory that
/// already exists is left as it is, since it may be one the user chose.
pub(crate) fn create_dir_secure(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        b.mode(0o700);
        b.create(dir)?;
        // The umask cannot widen 0700, but set it explicitly so the mode
        // does not depend on how the directory came to exist.
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    b.create(dir)?;
    Ok(())
}

/// Create a new file with mode 0600. Fails if it exists.
pub(crate) fn create_file_secure(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

/// Path of the segment starting at `first_seq` in `dir`.
pub(crate) fn path(dir: &Path, first_seq: u64, compressed: bool) -> PathBuf {
    dir.join(file_name(first_seq, compressed))
}

/// Compress the plain segment `first_seq` in `dir`. Returns the size of the
/// compressed file. The plain file is removed only after the compressed one
/// is in place.
pub(crate) fn compress(dir: &Path, first_seq: u64) -> io::Result<u64> {
    let plain = path(dir, first_seq, false);
    let gz = path(dir, first_seq, true);
    let tmp = dir.join(format!("{}.tmp", file_name(first_seq, true)));
    let _ = fs::remove_file(&tmp);
    let result = (|| {
        let mut src = File::open(&plain)?;
        let mut enc = GzEncoder::new(create_file_secure(&tmp)?, Compression::default());
        io::copy(&mut src, &mut enc)?;
        let f = enc.finish()?;
        f.sync_all()?;
        fs::rename(&tmp, &gz)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    fs::remove_file(&plain)?;
    Ok(fs::metadata(&gz)?.len())
}

/// One segment found on disk.
#[derive(Debug, Clone)]
pub(crate) struct Found {
    pub first_seq: u64,
    pub compressed: bool,
    pub bytes: u64,
    /// Modification time in ms since the epoch: when it was last written.
    pub mtime_ms: u64,
}

/// List the segments in `dir`, sorted by first sequence number. When both
/// names exist for one segment (a crash between rename and remove), the
/// plain file is removed and the compressed one kept.
pub(crate) fn list(dir: &Path) -> io::Result<Vec<Found>> {
    let mut found: BTreeMap<u64, Found> = BTreeMap::new();
    for e in fs::read_dir(dir)? {
        let e = e?;
        let name = e.file_name();
        let Some((first_seq, compressed)) = name.to_str().and_then(parse_name) else {
            continue;
        };
        let meta = e.metadata()?;
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        let f = Found {
            first_seq,
            compressed,
            bytes: meta.len(),
            mtime_ms,
        };
        match found.get(&first_seq) {
            Some(prev) if prev.compressed => {
                let _ = fs::remove_file(path(dir, first_seq, false));
            }
            Some(_) => {
                let _ = fs::remove_file(path(dir, first_seq, false));
                found.insert(first_seq, f);
            }
            None => {
                found.insert(first_seq, f);
            }
        }
    }
    Ok(found.into_values().collect())
}

/// Write the header line to a freshly created segment file.
pub(crate) fn write_header(
    w: &mut impl Write,
    kind: HeaderKind,
    tags: &BTreeMap<String, String>,
) -> io::Result<u64> {
    let h = header_line_tagged(kind, tags);
    w.write_all(h.as_bytes())?;
    w.write_all(b"\n")?;
    Ok(h.len() as u64 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{Payload, Provenance, Sample, Value};
    use crate::testutil::TempDir;
    use crate::EntityKey;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::fs;
    use std::io::Write;

    fn rec(seq: u64) -> Record {
        Record {
            seq,
            t_mono_ns: seq,
            t_wall_ms: seq,
            source: "t".into(),
            payload: Payload::Sample(Sample {
                entity: EntityKey::chip("0000:03:00.0"),
                signal: "n".into(),
                value: Value::U64(seq),
                prov: Provenance::Measured,
            }),
        }
    }

    /// Header, records 1 and 2, a corrupt line, record 4, then record 5 cut
    /// off half way with no trailing newline.
    fn damaged_body() -> Vec<u8> {
        let mut s = String::new();
        s.push_str(&header_line(HeaderKind::Live));
        s.push('\n');
        for q in [1, 2] {
            s.push_str(&rec(q).to_line());
            s.push('\n');
        }
        s.push_str("{\"q\":3,\"m\":garbage\n");
        s.push_str(&rec(4).to_line());
        s.push('\n');
        let five = rec(5).to_line();
        s.push_str(&five[..five.len() / 2]);
        s.into_bytes()
    }

    fn seqs(r: &SegmentRead) -> Vec<u64> {
        r.records.iter().map(|r| r.seq).collect()
    }

    #[test]
    fn names_round_trip() {
        assert_eq!(file_name(42, false), "bus-42.jsonl");
        assert_eq!(file_name(42, true), "bus-42.jsonl.gz");
        assert_eq!(parse_name("bus-42.jsonl"), Some((42, false)));
        assert_eq!(parse_name("bus-42.jsonl.gz"), Some((42, true)));
        for bad in [
            "bus-.jsonl",
            "bus-x.jsonl",
            "bus-1.json",
            "other",
            "bus-1.jsonl.gz.tmp",
        ] {
            assert_eq!(parse_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn corrupt_and_truncated_lines_are_skipped_and_counted() {
        let dir = TempDir::new("corrupt");
        fs::create_dir_all(dir.path()).unwrap();
        let plain = dir.path().join(file_name(1, false));
        fs::write(&plain, damaged_body()).unwrap();
        let gz = dir.path().join(file_name(1, true));
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&damaged_body()).unwrap();
        fs::write(&gz, enc.finish().unwrap()).unwrap();

        for path in [&plain, &gz] {
            let r = read_segment(path).unwrap();
            assert!(r.header.is_some(), "{path:?}");
            // Read up to the corrupt line and past it.
            assert_eq!(seqs(&r), vec![1, 2, 4], "{path:?}");
            // The corrupt middle line and the truncated last line.
            assert_eq!(r.bad_lines, 2, "{path:?}");
        }
    }

    #[test]
    fn read_from_skips_earlier_records_and_stops_at_max() {
        let dir = TempDir::new("from");
        fs::create_dir_all(dir.path()).unwrap();
        let plain = dir.path().join(file_name(1, false));
        fs::write(&plain, damaged_body()).unwrap();
        let r = read_segment_from(&plain, 2, 2).unwrap();
        assert_eq!(seqs(&r), vec![2, 4]);
    }
}
