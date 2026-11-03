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
//! Nothing is written after the window closes: [`FRAME_WINDOW`] after the
//! first frame, or 30 s after start when no frame is drawn (`--headless`), so
//! a variable left set does not grow the file for the whole session.
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

/// Open the log for appending: a regular file only. The open does not block
/// (a FIFO with no reader would otherwise hold start-up), and anything that
/// is not a regular file is refused.
fn open_log(path: &std::path::Path) -> Option<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NONBLOCK);
    }
    let f = opts.open(path).ok()?;
    f.metadata().ok()?.is_file().then_some(f)
}

/// How long after start the log is written when no frame is ever drawn
/// (`--headless` runs the same backend code).
const NO_FRAME_WINDOW: Duration = Duration::from_secs(30);

/// Whether `now` is still inside the logging window: [`FRAME_WINDOW`] after
/// the first frame, or [`NO_FRAME_WINDOW`] after start when there is none.
fn in_window(now: Instant, start: Instant, first_frame: Option<Instant>) -> bool {
    match first_frame {
        Some(f) => now.saturating_duration_since(f) <= FRAME_WINDOW,
        None => now.saturating_duration_since(start) <= NO_FRAME_WINDOW,
    }
}

fn write_line(line: std::fmt::Arguments<'_>) {
    if let Ok(mut g) = FILE.lock() {
        if let Some(f) = g.as_mut() {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// Note that a start-up phase was reached.
pub fn phase(name: &str) {
    if !enabled() || !in_window(Instant::now(), started(), FIRST_FRAME.get().copied()) {
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

    #[test]
    fn the_window_closes_after_the_first_frame_or_after_start() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        assert!(in_window(t0 + s(10), t0, None));
        assert!(!in_window(t0 + s(31), t0, None), "no frame: 30 s");
        assert!(in_window(t0 + s(6), t0, Some(t0 + s(2))));
        assert!(
            !in_window(t0 + s(8), t0, Some(t0 + s(2))),
            "5 s after the first frame"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let td = tempfile::tempdir().unwrap();
        let fifo = td.path().join("log");
        let ok = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(ok.success());
        // Without O_NONBLOCK this open would wait for a reader for ever.
        assert!(open_log(&fifo).is_none());
        assert!(open_log(&td.path().join("plain")).is_some());
    }
}
