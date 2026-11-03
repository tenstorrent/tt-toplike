// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! A panic during start-up, with the splash running, stops the splash before
//! the panic message is printed. Before this, the splash kept drawing and its
//! exit cleared the screen after the message, so the user saw nothing.
//!
//! Panic hooks are global to the process, so this has its own test binary.

use std::panic::{self, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use tt_toplike::ui::tui::splash;

/// Times the stand-in default hook ran, and how many of those saw the
/// splash still running.
static PRINTED: AtomicUsize = AtomicUsize::new(0);
static SPLASH_UP_AT_PRINT: AtomicUsize = AtomicUsize::new(0);

#[test]
fn a_start_up_panic_stops_the_splash_before_the_message() {
    // The hook that prints the message (the default one, in real use).
    panic::set_hook(Box::new(|_| {
        PRINTED.fetch_add(1, Ordering::SeqCst);
        if splash::is_running() {
            SPLASH_UP_AT_PRINT.fetch_add(1, Ordering::SeqCst);
        }
    }));

    // A panic before the TUI hook exists (in backend creation): only the
    // splash's own hook stands in front of the printer.
    let g = splash::start();
    assert!(splash::is_running());
    assert!(catch_unwind(|| panic!("backend creation failed")).is_err());
    assert_eq!(PRINTED.load(Ordering::SeqCst), 1);
    assert_eq!(
        SPLASH_UP_AT_PRINT.load(Ordering::SeqCst),
        0,
        "splash up at print"
    );
    assert!(!splash::is_running());
    drop(g);

    // A panic after the TUI hook is installed, as in run_app's set-up.
    let g = splash::start();
    tt_toplike::ui::tui::install_panic_hook();
    assert!(catch_unwind(|| panic!("run_app set-up failed")).is_err());
    assert_eq!(PRINTED.load(Ordering::SeqCst), 2);
    assert_eq!(
        SPLASH_UP_AT_PRINT.load(Ordering::SeqCst),
        0,
        "splash up at print"
    );
    drop(g);
}
