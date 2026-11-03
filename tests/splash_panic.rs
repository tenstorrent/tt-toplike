// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! A panic during start-up, with the splash on screen, leaves the panic
//! message readable: the splash is stopped before the message is printed,
//! nothing clears the screen, and nothing is written after the message.
//!
//! The splash's output and the TUI hook's terminal restore are captured into
//! a byte buffer (`splash::capture_for_test`), and the stand-in for the
//! default hook appends a marker to the same buffer, so the test reads the
//! order of everything the terminal would have received.
//!
//! Panic hooks are global to the process, so this has its own test binary,
//! and its cases run in order inside one test.

use std::panic::{self, catch_unwind};
use std::sync::{Arc, Mutex, OnceLock};
use tt_toplike::ui::tui::splash;

/// What the stand-in default hook writes as "the panic message".
const MSG: &[u8] = b"<PANIC MESSAGE>";
/// What the TUI hook's restore writes into the capture when the splash
/// thread was still alive at that moment (see `splash::emit_for_hook`).
const ALIVE: &[u8] = b"<SPLASH ALIVE>";
/// A clear-screen sequence: on a panic path it erases the message.
const CLEAR: &[u8] = b"\x1b[2J";

static BUF: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
/// Whether the splash thread was still alive when the message printed.
static ALIVE_AT_PRINT: Mutex<Vec<bool>> = Mutex::new(Vec::new());

fn buf() -> Vec<u8> {
    BUF.get().unwrap().lock().unwrap().clone()
}

fn clear_buf() {
    BUF.get().unwrap().lock().unwrap().clear();
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

/// Wait, without a deadline, until `needle` is in the capture. A broken
/// build hangs here and the CI job timeout ends it.
fn wait_for(needle: &[u8]) {
    while find(&buf(), needle).is_none() {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// After the splash's last draw: no clear, the message, then nothing.
fn assert_message_survives(case: &str) {
    let b = buf();
    let last_draw = rfind(&b, b"tt-toplike").expect("the splash drew");
    let msg = find(&b, MSG).unwrap_or_else(|| panic!("{case}: no message"));
    assert!(msg > last_draw, "{case}: the splash drew after the message");
    assert!(
        find(&b[last_draw..], CLEAR).is_none(),
        "{case}: the screen was cleared after the splash's last draw: {:?}",
        String::from_utf8_lossy(&b[last_draw..])
    );
    assert_eq!(msg + MSG.len(), b.len(), "{case}: bytes after the message");
}

#[test]
fn a_start_up_panic_leaves_the_message_readable() {
    BUF.set(splash::capture_for_test()).unwrap();
    // The hook that prints the message (the default one, in real use).
    panic::set_hook(Box::new(|_| {
        ALIVE_AT_PRINT.lock().unwrap().push(splash::thread_alive());
        BUF.get().unwrap().lock().unwrap().extend_from_slice(MSG);
    }));

    // A: the splash has drawn, the TUI hook does not exist yet (a panic in
    // backend creation). Only the splash's own hook stands in front.
    let g = splash::start();
    wait_for(b"tt-toplike");
    assert!(catch_unwind(|| panic!("backend creation failed")).is_err());
    assert_message_survives("A");
    assert!(
        find(&buf(), b"\x1b[?1049l").is_some(),
        "A: left the splash's screen"
    );
    assert_eq!(
        ALIVE_AT_PRINT.lock().unwrap().pop(),
        Some(false),
        "A: splash alive at print"
    );
    drop(g);
    assert_message_survives("A, after the guard dropped");

    // S-new-1: a panic inside the splash thread's own draw. The hook runs on
    // the splash thread and must finish and print. Then installing the TUI
    // hook (which takes the current hook) must not hang.
    clear_buf();
    splash::panic_in_next_draw_for_test();
    let g = splash::start();
    wait_for(MSG);
    tt_toplike::ui::tui::install_panic_hook();
    drop(g);
    ALIVE_AT_PRINT.lock().unwrap().clear();

    // B: the TUI hook is installed and the splash has drawn and been handed
    // over (a panic in run_app's set-up).
    clear_buf();
    let g = splash::start();
    wait_for(b"tt-toplike");
    splash::handover();
    assert!(catch_unwind(|| panic!("run_app set-up failed")).is_err());
    assert_message_survives("B");
    assert!(
        find(&buf(), ALIVE).is_none(),
        "B: the TUI hook restored the screen while the splash could still draw"
    );
    assert_eq!(
        ALIVE_AT_PRINT.lock().unwrap().pop(),
        Some(false),
        "B: splash alive at print"
    );
    drop(g);
    assert_message_survives("B, after the guard dropped");
}
