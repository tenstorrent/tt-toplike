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
//! A file that cannot be opened turns the log off; the TUI never fails
//! because of it.
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
            let file = std::env::var_os(ENV).and_then(|p| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)
                    .ok()
            });
            let on = file.is_some();
            if let Ok(mut g) = FILE.lock() {
                *g = file;
            }
            STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
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
    if !enabled() {
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
