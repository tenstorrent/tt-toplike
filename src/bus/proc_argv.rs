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
//! inference runtime. At most [`MAX_ENRICHED`] are read per call.
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

/// Build the process source's snapshots from the panel's rows, with the
/// command line and start time read from `/proc` on Linux.
///
/// The first [`MAX_ENRICHED`] rows are read; any row past that, a process
/// that exited in between, or a file that cannot be read keeps the empty
/// command line and start time 0 that `ProcSnapshot::from` gives. On other
/// platforms nothing is read.
pub fn snapshots_from_rows(rows: &[ProcRow]) -> Vec<ProcSnapshot> {
    rows.iter()
        .enumerate()
        .map(|(i, r)| {
            let mut s = ProcSnapshot::from(r);
            if i < MAX_ENRICHED {
                enrich(&mut s);
            }
            s
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn enrich(s: &mut ProcSnapshot) {
    if s.pid == 0 {
        return;
    }
    let dir = std::path::PathBuf::from(format!("/proc/{}", s.pid));
    // Read the start time first and again after the command line: if the
    // pid was recycled in between, the two differ and nothing is kept.
    let start = std::fs::read_to_string(dir.join("stat"))
        .ok()
        .and_then(|t| parse_stat_start_ticks(&t));
    let Some(start) = start else {
        return;
    };
    let argv = std::fs::read(dir.join("cmdline"))
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

#[cfg(not(target_os = "linux"))]
fn enrich(_s: &mut ProcSnapshot) {}

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
}
