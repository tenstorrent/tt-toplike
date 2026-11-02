// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The TUI's panic hook and the bus's quiet reducer hook, in the order
//! `run_tui` installs them (`start_bus_after_panic_hook`).
//!
//! The panic hook is global to the process, so this lives in its own test
//! binary: no other test can install a hook or panic while it runs.
//!
//! A counting hook stands in for the default one (it is what the TUI hook
//! wraps). The TUI hook re-enables stderr logging before it calls the hook
//! it wraps, so `logging::stderr_enabled()` going from false to true shows
//! that the TUI hook ran.

use std::panic::{self, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use tt_toplike::backend::mock::MockBackend;
use tt_toplike::backend::TelemetryBackend;
use tt_toplike::bus::tui_bus::{Attach, TuiBusOptions};
use tt_toplike_bus::{
    Bus, BusConfig, EntityKey, Payload, Provenance, Record, Reducer, Sample, Value,
};

static CALLS: AtomicUsize = AtomicUsize::new(0);

struct Panics;

impl Reducer for Panics {
    fn apply(&mut self, _: &Record) {
        panic!("reducer bug");
    }
}

fn sample() -> Payload {
    Payload::Sample(Sample {
        entity: EntityKey::chip("0000:01:00.0"),
        signal: "n".into(),
        value: Value::U64(1),
        prov: Provenance::Measured,
    })
}

#[test]
fn a_reducer_panic_is_silent_and_an_ordinary_panic_reaches_the_tui_hook() {
    panic::set_hook(Box::new(|_| {
        CALLS.fetch_add(1, Ordering::SeqCst);
    }));
    let mut backend = MockBackend::new(1);
    backend.init().unwrap();
    backend.update().unwrap();
    let opts = TuiBusOptions {
        bus: BusConfig::default(),
        serve_socket: None,
        attach: Attach::Off,
        synthetic: true,
        relay: false,
        gozer: false,
        local_workloads: false,
    };
    // As run_tui does: the TUI's hook, then the bus (which installs the
    // quiet reducer hook around it).
    let bus = tt_toplike::ui::tui::start_bus_after_panic_hook(&opts, &backend).unwrap();
    assert!(bus.host().is_some());

    // The TUI disables stderr logging while it draws.
    tt_toplike::logging::disable_stderr();

    // A reducer panic: the bus catches it and goes on; nothing reaches the
    // TUI hook (stderr stays off) or the hook behind it.
    let b = Bus::open_with(BusConfig::default(), Box::new(Panics)).unwrap();
    assert_eq!(b.publish("t", sample()), 1);
    assert_eq!(b.publish("t", sample()), 2, "publish keeps working");
    assert_eq!(
        CALLS.load(Ordering::SeqCst),
        0,
        "a reducer panic was printed"
    );
    assert!(
        !tt_toplike::logging::stderr_enabled(),
        "a reducer panic ran the TUI hook (it would restore the terminal mid-session)"
    );

    // An ordinary panic still reaches the TUI hook, which restores the
    // terminal and passes it on.
    assert!(catch_unwind(|| panic!("ordinary")).is_err());
    assert!(tt_toplike::logging::stderr_enabled(), "the TUI hook ran");
    assert_eq!(CALLS.load(Ordering::SeqCst), 1);
}
