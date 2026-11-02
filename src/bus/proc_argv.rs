//! Command lines and start times for the process source, read from `/proc`.
//!
//! `ProcRow` (the process panel's row) has neither a command line nor a
//! start time, so `ProcSnapshot::from(&ProcRow)` leaves both empty. Without a
//! start time a recycled pid looks like the same process, and without a
//! command line the bus cannot tell two `python` processes apart. The host
//! fills both in here for the processes it hands to `ProcessSource`.
//!
//! Only the rows it is given are read: the process panel's rows, which are
//! the processes that hold a `/dev/tenstorrent` device or matched an
//! inference runtime. At most [`MAX_ENRICHED`] are read per call, and at
//! most [`MAX_CMDLINE_BYTES`] of each command line (whole arguments only).
//!
//! # Redaction and argument boundaries
//!
//! `/proc/<pid>/cmdline` is the argument vector separated by NUL bytes, so
//! the true argument boundaries are known here. The redaction API
//! ([`crate::bus::redact::redact_cmdline`]) takes one string, so the
//! arguments are joined with single spaces and redacted as the rest of the
//! code base does. An argument that itself holds a space is then split by
//! the redactor's word pass, which is what the redactor's quote handling is
//! built for (see the `redact` module docs). Redaction happens later, in
//! `ProcessSource`, never here: this module keeps the raw text only long
//! enough to hand it over.
//!
//! Everything here is plain parsing plus two file reads per pid. Nothing
//! opens a device.

use crate::bus::workload_sources::ProcSnapshot;
use crate::workload::host_processes::ProcRow;

/// Most processes whose `/proc` files are read in one call.
pub const MAX_ENRICHED: usize = 128;

/// Split the bytes of `/proc/<pid>/cmdline` into arguments.
///
/// Arguments are separated by NUL. The kernel ends the last argument with a
/// NUL too, so a trailing empty piece is dropped; an empty argument in the
/// middle (`a\0\0b`) is kept. Bytes that are not UTF-8 become U+FFFD. A
/// process that rewrote its title with spaces and no NULs (some daemons do)
/// comes back as one argument.
pub fn parse_cmdline(bytes: &[u8]) -> Vec<String> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let body = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    body.split(|b| *b == 0)
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect()
}

/// The start time (field 22, clock ticks since boot) from the text of
/// `/proc/<pid>/stat`.
///
/// Field 2 is the command name in parentheses and may itself hold spaces
/// and `)`, so the fields are counted from the LAST `)`. `None` when the text
/// does not have that shape.
pub fn parse_stat_start_ticks(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // After the `)` the next field is field 3 (state), so field 22 is the
    // 20th word, index 19.
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// Most bytes of `/proc/<pid>/cmdline` read per process. A command line can
/// be as long as `ARG_MAX` (megabytes); this bounds the read cost per tick.
pub const MAX_CMDLINE_BYTES: usize = 64 * 1024;

/// Build the process source's snapshots from the panel's rows, with the
/// command line and start time read from `/proc` on Linux.
///
/// The first [`MAX_ENRICHED`] rows are read; any row past that, a process
/// that exited in between, or a file that cannot be read keeps the empty
/// command line and start time 0 that `ProcSnapshot::from` gives. On other
/// platforms nothing is read.
pub fn snapshots_from_rows(rows: &[ProcRow]) -> Vec<ProcSnapshot> {
    #[cfg(target_os = "linux")]
    {
        snapshots_from_rows_in(std::path::Path::new("/proc"), rows)
    }
    #[cfg(not(target_os = "linux"))]
    {
        rows.iter().map(ProcSnapshot::from).collect()
    }
}

/// [`snapshots_from_rows`] reading `<root>/<pid>/{stat,cmdline}`. Tests
/// pass a directory of fixture files.
pub fn snapshots_from_rows_in(root: &std::path::Path, rows: &[ProcRow]) -> Vec<ProcSnapshot> {
    rows.iter()
        .enumerate()
        .map(|(i, r)| {
            let mut s = ProcSnapshot::from(r);
            if i < MAX_ENRICHED {
                enrich(root, &mut s);
            }
            s
        })
        .collect()
}

/// Read at most [`MAX_CMDLINE_BYTES`] of a `cmdline` file. When the file
/// is longer, the argument cut at the limit is dropped (the text is cut
/// after the last NUL), so every argument kept is whole.
fn read_cmdline(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    read_bounded(std::fs::File::open(path)?)
}

/// The body of [`read_cmdline`]: pulls at most `MAX_CMDLINE_BYTES + 1`
/// bytes from `r` (one more than kept, to tell a cut line from one that
/// fits exactly).
fn read_bounded(r: impl std::io::Read) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    r.take(MAX_CMDLINE_BYTES as u64 + 1).read_to_end(&mut buf)?;
    if buf.len() > MAX_CMDLINE_BYTES {
        buf.truncate(MAX_CMDLINE_BYTES);
        let keep = buf.iter().rposition(|b| *b == 0).map_or(0, |i| i + 1);
        buf.truncate(keep);
    }
    Ok(buf)
}

fn enrich(root: &std::path::Path, s: &mut ProcSnapshot) {
    if s.pid == 0 {
        return;
    }
    let dir = root.join(s.pid.to_string());
    // Read the start time first and again after the command line: if the
    // pid was recycled in between, the two differ and nothing is kept.
    let start = std::fs::read_to_string(dir.join("stat"))
        .ok()
        .and_then(|t| parse_stat_start_ticks(&t));
    let Some(start) = start else {
        return;
    };
    let argv = read_cmdline(&dir.join("cmdline"))
        .map(|b| parse_cmdline(&b))
        .unwrap_or_default();
    let again = std::fs::read_to_string(dir.join("stat"))
        .ok()
        .and_then(|t| parse_stat_start_ticks(&t));
    if again != Some(start) {
        return;
    }
    s.start_ticks = start;
    if !argv.is_empty() {
        s.cmdline = argv.join(" ");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmdline_splits_on_nul_and_keeps_quotes_and_spaces_inside_an_argument() {
        let raw = b"python3\0-c\0print(\"a b\")\0--api-key=sk-x\0\0last\0";
        assert_eq!(
            parse_cmdline(raw),
            vec![
                "python3",
                "-c",
                "print(\"a b\")",
                "--api-key=sk-x",
                "",
                "last"
            ]
        );
    }

    #[test]
    fn cmdline_without_a_trailing_nul_or_with_bad_utf8_still_parses() {
        assert_eq!(parse_cmdline(b"a\0b"), vec!["a", "b"]);
        assert_eq!(
            parse_cmdline(b"nginx: worker process"),
            vec!["nginx: worker process"]
        );
        assert_eq!(parse_cmdline(b"x\xff\0"), vec!["x\u{fffd}"]);
        assert!(parse_cmdline(b"").is_empty());
    }

    #[test]
    fn stat_start_time_is_field_22_counted_after_the_last_paren() {
        // A command name holding spaces and a `)` (field 2), then fields 3..22.
        let stat = "4242 (my ) proc) S 1 4242 4242 0 -1 4194560 100 0 0 0 \
                    5 6 0 0 20 0 3 0 987654 123456 789 18446744073709551615";
        assert_eq!(parse_stat_start_ticks(stat), Some(987_654));
        assert_eq!(parse_stat_start_ticks("garbage"), None);
        assert_eq!(parse_stat_start_ticks("1 (x) S 1 2"), None);
    }

    /// This test's own process: the start time and command line come from
    /// `/proc/self`'s files, read through the row path.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_row_for_a_live_process_gets_its_start_time_and_command_line() {
        let pid = std::process::id();
        let row = ProcRow {
            pid: pid as i32,
            name: "test".into(),
            cpu_pct: 0.0,
            mem_bytes: 0,
            inference: None,
            active: false,
            tt: None,
        };
        let snaps = snapshots_from_rows(&[row]);
        let want_start =
            parse_stat_start_ticks(&std::fs::read_to_string("/proc/self/stat").unwrap());
        assert_eq!(Some(snaps[0].start_ticks), want_start);
        let argv0 = std::env::args().next().unwrap();
        assert!(snaps[0].cmdline.starts_with(&argv0), "{}", snaps[0].cmdline);
    }

    #[test]
    fn rows_past_the_cap_are_not_read() {
        let pid = std::process::id() as i32;
        let rows: Vec<ProcRow> = (0..MAX_ENRICHED + 1)
            .map(|_| ProcRow {
                pid,
                name: "x".into(),
                cpu_pct: 0.0,
                mem_bytes: 0,
                inference: None,
                active: false,
                tt: None,
            })
            .collect();
        let snaps = snapshots_from_rows(&rows);
        assert_eq!(snaps.len(), MAX_ENRICHED + 1);
        assert!(snaps[MAX_ENRICHED].cmdline.is_empty());
        assert_eq!(snaps[MAX_ENRICHED].start_ticks, 0);
    }

    /// A 1 MiB command line is read only up to 64 KiB, and the argument cut
    /// in half at the limit is dropped, so every argument kept is whole.
    #[test]
    fn a_huge_command_line_is_read_only_up_to_the_limit() {
        let root = tempfile::tempdir().unwrap();
        let pid = 4242u32;
        let dir = root.path().join(pid.to_string());
        std::fs::create_dir(&dir).unwrap();
        let stat = "4242 (big) S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 777 0 0";
        std::fs::write(dir.join("stat"), stat).unwrap();
        let arg = "x".repeat(99);
        let mut cmd = Vec::new();
        while cmd.len() < 1 << 20 {
            cmd.extend_from_slice(arg.as_bytes());
            cmd.push(0);
        }
        std::fs::write(dir.join("cmdline"), &cmd).unwrap();
        let row = ProcRow {
            pid: pid as i32,
            name: "big".into(),
            cpu_pct: 0.0,
            mem_bytes: 0,
            inference: None,
            active: false,
            tt: None,
        };
        let s = &snapshots_from_rows_in(root.path(), &[row])[0];
        assert_eq!(s.start_ticks, 777);
        assert!(s.cmdline.len() <= MAX_CMDLINE_BYTES, "{}", s.cmdline.len());
        let args: Vec<&str> = s.cmdline.split(' ').collect();
        assert_eq!(args.len(), MAX_CMDLINE_BYTES / 100);
        assert!(
            args.iter().all(|a| *a == arg),
            "a partial argument was kept"
        );
    }

    /// The read itself is bounded: no more than the limit plus one byte is
    /// pulled from a 1 MiB command line.
    #[test]
    fn the_cmdline_read_pulls_at_most_the_limit_plus_one_byte() {
        struct Counting<R>(R, usize);
        impl<R: std::io::Read> std::io::Read for Counting<R> {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                let n = self.0.read(b)?;
                self.1 += n;
                Ok(n)
            }
        }
        let big = vec![b'a'; 1 << 20];
        let mut c = Counting(std::io::Cursor::new(big), 0);
        let got = read_bounded(&mut c).unwrap();
        assert!(c.1 <= MAX_CMDLINE_BYTES + 1, "pulled {} bytes", c.1);
        // No NUL at all within the limit: nothing whole to keep.
        assert!(got.is_empty());
        // A line that fits is kept whole.
        assert_eq!(read_bounded(&b"a\0b\0"[..]).unwrap(), b"a\0b\0");
    }
}
