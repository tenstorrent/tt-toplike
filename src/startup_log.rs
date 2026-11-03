// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! A timeline of the TUI's start-up, written only when asked for.
//!
//! Set `TT_TOPLIKE_STARTUP_LOG=<file>` and the TUI appends one line per
//! start-up phase to that file: milliseconds since `run_tui` began, then the
//! phase name. For the first [`FRAME_WINDOW`] after the first frame it also
//! writes one line per frame with the time spent drawing and the time spent
//! in the rest of the loop iteration, so a slow first second can be traced
//! to the draw, to the loop's own work, or to waiting for the CPU.
//!
//! Without the variable every call is one relaxed atomic load and returns.
//! The file is opened once, on the first call that finds the variable set.
//! A file that cannot be opened, or a path that is not a regular file (a
//! FIFO, a device), turns the log off; the TUI never fails because of it.
//!
//! A path that exists and is not a regular file is refused before it is
//! opened. Nothing is written after the window closes, [`FRAME_WINDOW`] after
//! the first frame; before the first frame the log goes on however slow the
//! start is. The file never gets more than [`MAX_LINES`] lines, so a
//! variable left set (or `--headless`, which draws no frame) cannot grow it
//! for the whole session.
//!
//! The clock starts at [`begin`]. Calls made before it count from the first
//! call. It lives at the crate root, outside the TUI, so the backend and the
//! workload threads can mark their own phases on the same clock.

use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The environment variable that names the log file.
pub const ENV: &str = "TT_TOPLIKE_STARTUP_LOG";

/// How long after the first frame per-frame lines are written.
pub const FRAME_WINDOW: Duration = Duration::from_secs(5);

/// 0: not decided yet, 1: off, 2: on.
static STATE: AtomicU8 = AtomicU8::new(0);
static START: OnceLock<Instant> = OnceLock::new();
static FIRST_FRAME: OnceLock<Instant> = OnceLock::new();
static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

/// Start the clock. Called first thing in `run_tui`.
pub fn begin() {
    let _ = START.get_or_init(Instant::now);
    phase("run_tui");
}

/// When the clock started.
pub fn started() -> Instant {
    *START.get_or_init(Instant::now)
}

fn enabled() -> bool {
    match STATE.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let file = std::env::var_os(ENV).and_then(|p| open_log(p.as_ref()));
            let on = file.is_some();
            if let Ok(mut g) = FILE.lock() {
                *g = file;
            }
            STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
    }
}

/// True when `path` exists and is not a regular file (following links): a
/// FIFO, a directory, a device node, a socket. Checked before the log is
/// opened, so such a path is never opened at all (a device node opened by
/// mistake would make this process a holder of that device).
fn non_regular_target(path: &std::path::Path) -> bool {
    match std::fs::metadata(path) {
        Ok(m) => !m.is_file(),
        // Missing: the log creates it. Any other error: the open fails too.
        Err(_) => false,
    }
}

/// Open the log for appending: a regular file only. A path that exists and
/// is not a regular file is refused before any open. The open itself does
/// not block (`O_NONBLOCK`, for a FIFO that appears between the check and
/// the open) and does not take a controlling terminal (`O_NOCTTY`); what it
/// opened is checked again.
fn open_log(path: &std::path::Path) -> Option<std::fs::File> {
    if non_regular_target(path) {
        return None;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY);
    }
    let f = opts.open(path).ok()?;
    f.metadata().ok()?.is_file().then_some(f)
}

/// Most lines written to one log. It bounds the file when no frame is ever
/// drawn (`--headless` runs the same backend code): at a few lines every
/// 2 s that is over an hour, about 0.5 MB.
const MAX_LINES: usize = 10_000;

/// Lines written so far.
static LINES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn under_line_cap(written: usize) -> bool {
    written < MAX_LINES
}

/// Whether `now` is still inside the logging window. Before the first frame
/// the window is open, however long start-up takes, so the lines that say
/// what a slow start waited on, and the first frame itself, are kept. After
/// the first frame it closes [`FRAME_WINDOW`] later.
fn in_window(now: Instant, first_frame: Option<Instant>) -> bool {
    match first_frame {
        Some(f) => now.saturating_duration_since(f) <= FRAME_WINDOW,
        None => true,
    }
}

fn write_line(line: std::fmt::Arguments<'_>) {
    if !under_line_cap(LINES.fetch_add(1, Ordering::Relaxed)) {
        return;
    }
    if let Ok(mut g) = FILE.lock() {
        if let Some(f) = g.as_mut() {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// Note that a start-up phase was reached.
pub fn phase(name: &str) {
    if !enabled() || !in_window(Instant::now(), FIRST_FRAME.get().copied()) {
        return;
    }
    let ms = started().elapsed().as_secs_f64() * 1000.0;
    let thread = std::thread::current();
    let tname = thread.name().unwrap_or("?");
    write_line(format_args!("{ms:9.1} {name} [{tname}]"));
}

/// Note one frame: how long the draw took and how long the rest of the loop
/// iteration before it took. Written for [`FRAME_WINDOW`] after the first
/// frame, then ignored.
pub fn frame(draw: Duration, work: Duration) {
    if !enabled() {
        return;
    }
    let now = Instant::now();
    let first = *FIRST_FRAME.get_or_init(|| now);
    if now.duration_since(first) > FRAME_WINDOW {
        return;
    }
    let ms = now.duration_since(started()).as_secs_f64() * 1000.0;
    write_line(format_args!(
        "{ms:9.1} frame draw={:.1}ms work={:.1}ms",
        draw.as_secs_f64() * 1000.0,
        work.as_secs_f64() * 1000.0
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No frame yet: the log goes on, however slow the start. After the
    /// first frame it stops after [`FRAME_WINDOW`].
    #[test]
    fn the_window_stays_open_until_a_frame_and_closes_after_it() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        assert!(in_window(t0 + s(10), None));
        assert!(in_window(t0 + s(45), None), "a 45 s start is still logged");
        assert!(in_window(t0 + s(6), Some(t0 + s(2))));
        assert!(
            !in_window(t0 + s(8), Some(t0 + s(2))),
            "5 s after the first frame"
        );
    }

    #[test]
    fn the_line_cap_bounds_the_file() {
        assert!(under_line_cap(0));
        assert!(under_line_cap(MAX_LINES - 1));
        assert!(!under_line_cap(MAX_LINES));
    }

    #[cfg(unix)]
    fn mkfifo(path: &std::path::Path) {
        let ok = std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .unwrap();
        assert!(ok.success());
    }

    /// The check made before any open: a FIFO, a directory, or a link to
    /// either is not a log file. A path that does not exist yet is fine
    /// (the log creates it), and so is a regular file.
    #[cfg(unix)]
    #[test]
    fn non_regular_targets_are_found_before_opening() {
        let td = tempfile::tempdir().unwrap();
        let fifo = td.path().join("fifo");
        mkfifo(&fifo);
        let link = td.path().join("link");
        std::os::unix::fs::symlink(&fifo, &link).unwrap();
        let plain = td.path().join("plain");
        std::fs::write(&plain, "").unwrap();
        assert!(non_regular_target(&fifo));
        assert!(non_regular_target(td.path()), "a directory");
        assert!(non_regular_target(&link), "a link to a FIFO");
        assert!(!non_regular_target(&plain));
        assert!(!non_regular_target(&td.path().join("new")));
    }

    /// A FIFO that has a reader would open for writing; it is refused all
    /// the same, and so is a directory. A missing path is created.
    #[cfg(unix)]
    #[test]
    fn open_log_refuses_a_fifo_with_a_reader_and_a_directory() {
        use std::os::unix::fs::OpenOptionsExt;
        let td = tempfile::tempdir().unwrap();
        let fifo = td.path().join("log");
        mkfifo(&fifo);
        let _reader = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)
            .unwrap();
        assert!(open_log(&fifo).is_none());
        assert!(open_log(td.path()).is_none());
        assert!(open_log(&td.path().join("plain")).is_some());
    }
}
