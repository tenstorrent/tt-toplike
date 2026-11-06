//! `install_quiet_reducer_hook` keeps a reducer panic off stderr and lets
//! every other panic through.
//!
//! The panic hook is global to the process, so this lives in its own test
//! binary: no other test can install a hook or panic while it runs.
//!
//! The previous hook is replaced by a counting one before the quiet hook is
//! installed. The count stands in for "something was printed": the default
//! hook prints exactly when it is called, and the quiet hook either calls
//! the previous hook or calls nothing.

use std::panic::{self, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tt_toplike_bus::{
    install_quiet_reducer_hook, Bus, BusConfig, EntityKey, Payload, Provenance, Record, Reducer,
    Sample, Value,
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
fn a_reducer_panic_is_silent_and_other_panics_still_reach_the_previous_hook() {
    panic::set_hook(Box::new(|_| {
        CALLS.fetch_add(1, Ordering::SeqCst);
    }));
    install_quiet_reducer_hook();
    // Installing twice must not stack a second wrapper.
    install_quiet_reducer_hook();

    let bus = Bus::open_with(BusConfig::default(), Box::new(Panics)).unwrap();
    assert_eq!(bus.publish("t", sample()), 1);
    assert_eq!(bus.publish("t", sample()), 2, "publish keeps working");
    assert_eq!(
        CALLS.load(Ordering::SeqCst),
        0,
        "reducer panics were printed"
    );

    // An ordinary panic on this thread still reaches the previous hook.
    let r = catch_unwind(|| panic!("ordinary"));
    assert!(r.is_err());
    assert_eq!(CALLS.load(Ordering::SeqCst), 1);

    // And one on another thread, while a reducer panics here.
    let flag = Arc::new(AtomicUsize::new(0));
    let f = flag.clone();
    let h = std::thread::spawn(move || {
        f.store(1, Ordering::SeqCst);
        panic!("other thread");
    });
    bus.publish("t", sample());
    assert!(h.join().is_err());
    assert_eq!(flag.load(Ordering::SeqCst), 1);
    assert_eq!(CALLS.load(Ordering::SeqCst), 2);

    // The flag is cleared after a reducer panic: the next ordinary panic on
    // this thread is reported.
    let _ = catch_unwind(|| panic!("after"));
    assert_eq!(CALLS.load(Ordering::SeqCst), 3);
}
