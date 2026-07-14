// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Collector trait + panic-isolated spawn helper. Each collector runs on its
//! own thread, pushes SniffEvents over a bounded channel, and stops when its
//! shutdown flag is set. A panicking collector is contained (status = Err) and
//! never takes down the engine or its peers.

use super::event::SniffEvent;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

pub type Tx = std::sync::mpsc::SyncSender<SniffEvent>;

#[derive(Debug, Clone, PartialEq)]
pub enum CollectorStatus {
    Ok,
    PermDenied(String),
    Err(String),
}

pub trait Collector: Send {
    fn name(&self) -> &'static str;
    /// Run until `shutdown` is set. Must poll `shutdown` regularly.
    fn run(&mut self, tx: Tx, shutdown: Arc<AtomicBool>);
}

pub struct Handle {
    pub name: &'static str,
    join: Option<JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
    pub status: Arc<Mutex<CollectorStatus>>,
}

impl Handle {
    pub fn stop(mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

pub fn spawn(mut c: Box<dyn Collector>, tx: Tx) -> Handle {
    let name = c.name();
    let shutdown = Arc::new(AtomicBool::new(false));
    let status = Arc::new(Mutex::new(CollectorStatus::Ok));
    let sd = shutdown.clone();
    let st = status.clone();
    let join = thread::Builder::new()
        .name(format!("hivemind-{name}"))
        .spawn(move || {
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| c.run(tx, sd)));
            if result.is_err() {
                *st.lock().unwrap() = CollectorStatus::Err("collector panicked".into());
            }
        })
        .expect("spawn hivemind collector thread");
    Handle {
        name,
        join: Some(join),
        shutdown,
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::hivemind::event::{EventKind, Severity, Source};
    use std::sync::mpsc::sync_channel;
    use std::time::{Duration, Instant};

    struct Panicky;
    impl Collector for Panicky {
        fn name(&self) -> &'static str {
            "panicky"
        }
        fn run(&mut self, _tx: Tx, _sd: Arc<AtomicBool>) {
            panic!("boom");
        }
    }

    struct OneShot;
    impl Collector for OneShot {
        fn name(&self) -> &'static str {
            "oneshot"
        }
        fn run(&mut self, tx: Tx, _sd: Arc<AtomicBool>) {
            let _ = tx.try_send(SniffEvent {
                ts: Instant::now(),
                source: Source::Host,
                device: None,
                severity: Severity::Info,
                kind: EventKind::Log,
                text: "hi".into(),
                origin: "oneshot".into(),
            });
        }
    }

    #[test]
    fn panic_is_contained_and_marks_status() {
        let (tx, _rx) = sync_channel(4);
        let h = spawn(Box::new(Panicky), tx);
        // Give the thread a moment to run and panic.
        thread::sleep(Duration::from_millis(50));
        let st = h.status.lock().unwrap().clone();
        h.stop();
        assert_eq!(st, CollectorStatus::Err("collector panicked".into()));
    }

    #[test]
    fn collector_can_emit_and_stop() {
        let (tx, rx) = sync_channel(4);
        let h = spawn(Box::new(OneShot), tx);
        let got = rx.recv_timeout(Duration::from_millis(200)).unwrap();
        assert_eq!(got.text, "hi");
        h.stop();
    }
}
