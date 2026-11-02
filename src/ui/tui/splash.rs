// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The start-up splash: a small box with the name, the version, a spinner,
//! the current start-up step and the time so far.
//!
//! # When it shows
//!
//! [`start`] is called first thing in `run_tui`, after the TTY check, so the
//! terminal is never blank while the backend starts. The splash draws only
//! when the first real frame is not ready within [`SHOW_AFTER`] (150 ms), so a
//! fast start does not flash it for one frame. [`first_frame`], called just
//! before the first `terminal.draw`, stops it and clears the screen for that
//! frame. `--headless`, `--serve` without a TTY and a stdout that is not a
//! terminal never reach `run_tui`'s TUI path, so they never draw it.
//!
//! # How it draws
//!
//! A thread of its own (`tt-toplike-splash`) redraws the box about every
//! 50 ms with plain escape sequences, without ratatui, so it moves while the
//! main thread is busy starting the backend. The first time it draws it
//! enters the alternate screen. What it shows is [`splash_lines`], a pure
//! function of the elapsed time, the step and whether the first frame is
//! ready, so it is tested without a terminal. It uses left and bottom bars
//! only, no right-side border characters.
//!
//! # Giving the terminal back
//!
//! * Normal start: `run_tui` enters raw mode and the alternate screen itself
//!   and calls [`handover`]; the first frame calls [`first_frame`], which
//!   stops the thread and clears the screen.
//! * A start that fails before [`handover`]: dropping the [`Guard`] returned
//!   by [`start`] leaves the alternate screen and shows the cursor, so the
//!   error is printed on the normal screen.
//! * Ctrl-C or SIGTERM while the splash is up and raw mode is not on yet
//!   (Unix): a signal handler writes the same restore sequence and then lets
//!   the signal end the process as before. It is removed at [`handover`].

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How long the first frame may take before the splash shows.
pub const SHOW_AFTER: Duration = Duration::from_millis(150);

/// How often the splash thread redraws.
const TICK: Duration = Duration::from_millis(50);

/// One spinner step lasts this long.
const SPIN_STEP_MS: u128 = 80;

/// Braille spinner frames.
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Width of the top and bottom bars, in cells.
const BAR_WIDTH: usize = 34;

/// What start-up is doing now, in words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Step {
    Starting = 0,
    LookingForChips = 1,
    ReadingTtSmi = 2,
    StartingBus = 3,
    ScanningProcesses = 4,
}

impl Step {
    /// The step in words, as the splash shows it.
    pub fn words(self) -> &'static str {
        match self {
            Step::Starting => "starting",
            Step::LookingForChips => "looking for chips",
            Step::ReadingTtSmi => "reading tt-smi",
            Step::StartingBus => "starting the bus",
            Step::ScanningProcesses => "scanning processes",
        }
    }

    fn from_u8(v: u8) -> Step {
        match v {
            1 => Step::LookingForChips,
            2 => Step::ReadingTtSmi,
            3 => Step::StartingBus,
            4 => Step::ScanningProcesses,
            _ => Step::Starting,
        }
    }
}

/// The splash text for one moment, or `None` when nothing is drawn: before
/// [`SHOW_AFTER`] and once the first frame is ready. Pure: the same inputs
/// give the same lines.
pub fn splash_lines(
    elapsed: Duration,
    step: Step,
    first_frame_ready: bool,
    version: &str,
) -> Option<[String; 4]> {
    if first_frame_ready || elapsed < SHOW_AFTER {
        return None;
    }
    let spin = SPINNER[(elapsed.as_millis() / SPIN_STEP_MS) as usize % SPINNER.len()];
    let bar = "═".repeat(BAR_WIDTH);
    Some([
        format!("╔{bar}"),
        format!("║  tt-toplike {version}"),
        format!("║  {spin} {}  {:.1} s", step.words(), elapsed.as_secs_f64()),
        format!("╚{bar}"),
    ])
}

// ── The thread ───────────────────────────────────────────────────────────────

/// The current step, readable from the splash thread.
static STEP: AtomicU8 = AtomicU8::new(Step::Starting as u8);

/// True while a splash thread exists, so [`first_frame`] costs one load
/// after the first frame.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// The running splash, if any.
static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);

struct Active {
    shared: Arc<Shared>,
    thread: std::thread::JoinHandle<()>,
}

#[derive(Default)]
struct State {
    /// The thread has drawn at least once (it is in the alternate screen).
    drew: bool,
    /// `run_tui` owns the terminal now (raw mode and its own alternate
    /// screen are on).
    handed_over: bool,
    /// Set to end the thread.
    stop: bool,
    /// The Unix signal handlers are installed.
    handlers: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

/// Set the step the splash shows.
pub fn set_step(step: Step) {
    STEP.store(step as u8, Ordering::Relaxed);
    crate::startup_log::phase(step.words());
}

/// Returned by [`start`]. Dropping it stops the splash; if the terminal was
/// not handed over yet, the screen is given back.
pub struct Guard(());

impl Drop for Guard {
    fn drop(&mut self) {
        stop();
    }
}

/// Start the splash thread. It draws nothing for [`SHOW_AFTER`].
pub fn start() -> Guard {
    let shared = Arc::new(Shared::default());
    let started = Instant::now();
    let s2 = Arc::clone(&shared);
    let thread = std::thread::Builder::new()
        .name("tt-toplike-splash".into())
        .spawn(move || run(&s2, started));
    if let Ok(thread) = thread {
        if let Ok(mut g) = ACTIVE.lock() {
            *g = Some(Active { shared, thread });
            RUNNING.store(true, Ordering::Release);
        }
    }
    Guard(())
}

/// `run_tui` has entered raw mode and its own alternate screen: the splash
/// keeps drawing, but it no longer restores the terminal on exit.
pub fn handover() {
    let Ok(g) = ACTIVE.lock() else { return };
    if let Some(a) = g.as_ref() {
        let mut st = a.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        st.handed_over = true;
        if st.handlers {
            restore_signals();
            st.handlers = false;
        }
    }
}

/// The first real frame is about to be drawn: stop the splash and clear what
/// it drew. One atomic load once the splash is gone.
pub fn first_frame() {
    if RUNNING.load(Ordering::Acquire) {
        stop();
        crate::startup_log::phase("first frame");
    }
}

/// Stop the thread and wait for it. It clears the screen, or gives the
/// terminal back when it was never handed over.
fn stop() {
    let active = ACTIVE.lock().ok().and_then(|mut g| g.take());
    RUNNING.store(false, Ordering::Release);
    let Some(a) = active else { return };
    {
        let mut st = a.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        st.stop = true;
    }
    a.shared.wake.notify_all();
    let _ = a.thread.join();
}

/// Restore sequence for a terminal the TUI never took over: clear, leave the
/// alternate screen, show the cursor.
const RESTORE: &[u8] = b"\x1b[2J\x1b[?1049l\x1b[?25h";

fn run(shared: &Shared, started: Instant) {
    let version = env!("CARGO_PKG_VERSION");
    let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if st.stop {
            if st.drew {
                let mut out = std::io::stdout().lock();
                if st.handed_over {
                    // Clear for the first frame (ratatui's buffer is empty, so
                    // it writes every cell it uses).
                    let _ = out.write_all(b"\x1b[2J");
                } else {
                    let _ = out.write_all(RESTORE);
                }
                let _ = out.flush();
            }
            if st.handlers {
                restore_signals();
                st.handlers = false;
            }
            return;
        }
        let step = Step::from_u8(STEP.load(Ordering::Relaxed));
        if let Some(lines) = splash_lines(started.elapsed(), step, false, version) {
            if !st.drew && !st.handed_over {
                st.handlers = install_signals();
            }
            draw(&lines, !st.drew);
            st.drew = true;
        }
        st = shared
            .wake
            .wait_timeout(st, TICK)
            .map(|(g, _)| g)
            .unwrap_or_else(|e| e.into_inner().0);
    }
}

/// Write the lines centred on the screen. The first draw enters the
/// alternate screen and hides the cursor. One write per draw, so it never
/// interleaves with the main thread's writes mid-line.
fn draw(lines: &[String; 4], first: bool) {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let width = lines
        .iter()
        .map(|l| unicode_width::UnicodeWidthStr::width(l.as_str()))
        .max()
        .unwrap_or(0) as u16;
    let left = cols.saturating_sub(width) / 2 + 1;
    let top = rows.saturating_sub(lines.len() as u16) / 2 + 1;
    let mut buf: Vec<u8> = Vec::with_capacity(512);
    if first {
        buf.extend_from_slice(b"\x1b[?1049h\x1b[?25l\x1b[2J");
    }
    for (i, line) in lines.iter().enumerate() {
        // Move, clear the line, then the text: teal bars, plain text.
        let _ = write!(buf, "\x1b[{};{}H\x1b[2K", top + i as u16, left);
        let (bar, text) = line.split_at(line.chars().next().map_or(0, char::len_utf8));
        let _ = write!(buf, "\x1b[38;2;79;209;197m{bar}\x1b[0m");
        if i == 1 {
            let _ = write!(buf, "\x1b[1m{text}\x1b[0m");
        } else if i == 3 || i == 0 {
            let _ = write!(buf, "\x1b[38;2;79;209;197m{text}\x1b[0m");
        } else {
            buf.extend_from_slice(text.as_bytes());
        }
    }
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(&buf);
    let _ = out.flush();
}

#[cfg(unix)]
mod signals {
    use std::sync::Mutex;

    /// The handlers that were in place before the splash's, by signal.
    static OLD: Mutex<Vec<(libc::c_int, libc::sigaction)>> = Mutex::new(Vec::new());

    const SIGNALS: [libc::c_int; 2] = [libc::SIGINT, libc::SIGTERM];

    /// Restore the terminal and end the process with the same signal.
    /// Only async-signal-safe calls: `write`, `signal`, `raise`.
    extern "C" fn on_signal(sig: libc::c_int) {
        let s = super::RESTORE;
        // SAFETY: write(2), signal(2) and raise(3) are async-signal-safe; the
        // buffer is a static byte string.
        unsafe {
            libc::write(1, s.as_ptr().cast(), s.len());
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }

    pub fn install() -> bool {
        let mut old = OLD.lock().unwrap_or_else(|e| e.into_inner());
        old.clear();
        for sig in SIGNALS {
            // SAFETY: a zeroed sigaction is valid; the handler only makes
            // async-signal-safe calls.
            unsafe {
                let mut new: libc::sigaction = std::mem::zeroed();
                new.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
                libc::sigemptyset(&mut new.sa_mask);
                let mut prev: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(sig, &new, &mut prev) == 0 {
                    old.push((sig, prev));
                }
            }
        }
        !old.is_empty()
    }

    pub fn restore() {
        let mut old = OLD.lock().unwrap_or_else(|e| e.into_inner());
        for (sig, prev) in old.drain(..) {
            // SAFETY: `prev` is the action sigaction(2) returned for `sig`.
            unsafe {
                libc::sigaction(sig, &prev, std::ptr::null_mut());
            }
        }
    }
}

#[cfg(unix)]
fn install_signals() -> bool {
    signals::install()
}

#[cfg(unix)]
fn restore_signals() {
    signals::restore()
}

#[cfg(not(unix))]
fn install_signals() -> bool {
    false
}

#[cfg(not(unix))]
fn restore_signals() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn nothing_shows_before_the_threshold() {
        assert!(splash_lines(ms(0), Step::LookingForChips, false, "1.0").is_none());
        assert!(splash_lines(ms(149), Step::LookingForChips, false, "1.0").is_none());
        assert!(splash_lines(ms(150), Step::LookingForChips, false, "1.0").is_some());
    }

    #[test]
    fn nothing_shows_once_the_first_frame_is_ready() {
        assert!(splash_lines(ms(5_000), Step::StartingBus, true, "1.0").is_none());
    }

    #[test]
    fn the_box_names_the_tool_the_step_and_the_time() {
        let l = splash_lines(ms(1_234), Step::ReadingTtSmi, false, "0.15.3").unwrap();
        assert_eq!(l[1], "║  tt-toplike 0.15.3");
        assert!(l[2].contains("reading tt-smi"), "{}", l[2]);
        assert!(l[2].ends_with("1.2 s"), "{}", l[2]);
    }

    #[test]
    fn the_spinner_moves() {
        let a = splash_lines(ms(200), Step::Starting, false, "x").unwrap();
        let b = splash_lines(ms(280), Step::Starting, false, "x").unwrap();
        let c = splash_lines(ms(200 + 80 * 10), Step::Starting, false, "x").unwrap();
        assert_ne!(a[2], b[2], "one spinner step later the glyph differs");
        assert_eq!(
            a[2].chars().nth(3),
            c[2].chars().nth(3),
            "the spinner has ten frames"
        );
    }

    #[test]
    fn every_step_has_words() {
        for v in 0..=4u8 {
            let s = Step::from_u8(v);
            assert_eq!(s as u8, v);
            assert!(!s.words().is_empty());
        }
    }

    /// Left and bottom bars only: no line ends in a border character.
    #[test]
    fn no_right_side_border() {
        let l = splash_lines(ms(500), Step::ScanningProcesses, false, "0.15.3").unwrap();
        for line in &l {
            let last = line.chars().last().unwrap();
            assert!(
                !"║│┃╗╝┐┘╮╯".contains(last),
                "line ends in a right-side border: {line:?}"
            );
        }
        assert!(l[0].starts_with('╔') && l[3].starts_with('╚'));
    }
}
