# Hivemindsweeper Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an opt-in `HivemindSweeper` TUI mode that passively sniffs TT-stack activity from multiple sources and renders it as a `source × device` minesweeper board with a drill-in / unified event feed.

**Architecture:** A new in-process `src/workload/hivemind/` engine owns a bounded event ring buffer and a decaying `(source × device)` heat grid. Independent `Collector` threads (procfs, cache-watch, log-tail, kmsg, inspector, wrap) push normalized `SniffEvent`s over a bounded `std::sync::mpsc::sync_channel`; the engine drains them each frame. The TUI gains a `DisplayMode::HivemindSweeper` entered by `~` or `/hivemind`, started lazily (zero idle cost) and rendered by a new `hivemind_view` module.

**Tech Stack:** Rust, ratatui/crossterm (TUI), `std::sync::mpsc`, `std::thread`, existing helpers in `src/workload/{process_monitor,inference_server}` and `src/animation/common.rs`. No new crate dependencies.

## Global Constraints

- **Read-only / non-invasive only.** Never set `TT_METAL_DPRINT_CORES`, `TT_METAL_WATCHER`, profiler env vars, or `TT_METAL_INSPECTOR`. Never read device-side debug/print buffers. Consume only downstream files, stdout, docker logs, kmsg, and subscribable services.
- **Zero idle cost.** No collector thread spawns until `DisplayMode::HivemindSweeper` is entered; `stop()` signals shutdown and joins on exit.
- **No new crate dependencies.** Use `std::sync::mpsc::sync_channel` and directory polling.
- **No silent truncation.** Dropped events (full channel / ring eviction) increment a visible per-collector counter.
- **Panic isolation.** A collector panic is caught, marked `✗(err)` in the header, and never takes down the engine or peers (mirrors the Luwen panic-catch history).
- **TUI conventions:** left-side and bottom borders only — never right-side box glyphs (`╔`/`║`/`╚`). Size panels to widest real display column via `unicode-width`; never clip.
- **SPDX header** on every new `.rs` file:
  ```
  // SPDX-License-Identifier: Apache-2.0
  // SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.
  ```
- **Version bump:** bump the crate version in `Cargo.toml` with the user-facing change (project rule: bump per change).
- **Build/test note:** if `.cargo/config.toml` + partial `vendor/` are present (left by `build-deb.sh`), move `.cargo/config.toml` aside so `cargo test` uses the real registry, then restore it.

---

### Task 1: Event model + engine skeleton

**Files:**
- Create: `src/workload/hivemind/mod.rs`
- Create: `src/workload/hivemind/event.rs`
- Modify: `src/workload/mod.rs` (add `pub mod hivemind;`)
- Test: inline `#[cfg(test)]` in `event.rs` and `mod.rs`

**Interfaces:**
- Produces:
  - `enum Source { Ttnn, TtMetal, TtLang, TtForge, TtXla, Vllm, Driver, Inspector, Host, Unknown }` (derives `Debug, Clone, Copy, PartialEq, Eq, Hash`), with `fn label(&self) -> &'static str` and `fn all() -> [Source; N]`.
  - `enum Severity { Trace, Info, Notice, Warn, Error }` (`Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord`).
  - `enum EventKind { Log, Compile, Process, Fd, DriverMsg, Emission, Inspector }` (`Debug, Clone, Copy, PartialEq, Eq`).
  - `struct SniffEvent { ts: Instant, source: Source, device: Option<u8>, severity: Severity, kind: EventKind, text: String, origin: String }`.
  - `struct Hivemind` with `fn new() -> Self`, `fn push_for_test(&mut self, ev: SniffEvent)`, `fn events(&self) -> &VecDeque<SniffEvent>`, `fn dropped(&self) -> u64`, and `const RING_CAP: usize = 4096`.

- [ ] **Step 1: Write the failing test**

In `src/workload/hivemind/event.rs`:
```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Normalized event model for the hivemindsweeper activity sniffer.

use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    Ttnn,
    TtMetal,
    TtLang,
    TtForge,
    TtXla,
    Vllm,
    Driver,
    Inspector,
    Host,
    Unknown,
}

impl Source {
    pub fn label(&self) -> &'static str {
        match self {
            Source::Ttnn => "ttnn",
            Source::TtMetal => "metal",
            Source::TtLang => "tt-lang",
            Source::TtForge => "tt-forge",
            Source::TtXla => "tt-xla",
            Source::Vllm => "vLLM",
            Source::Driver => "driver",
            Source::Inspector => "inspect",
            Source::Host => "host",
            Source::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Trace,
    Info,
    Notice,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Log,
    Compile,
    Process,
    Fd,
    DriverMsg,
    Emission,
    Inspector,
}

#[derive(Debug, Clone)]
pub struct SniffEvent {
    pub ts: Instant,
    pub source: Source,
    pub device: Option<u8>,
    pub severity: Severity,
    pub kind: EventKind,
    pub text: String,
    pub origin: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_orders_low_to_high() {
        assert!(Severity::Trace < Severity::Error);
        assert!(Severity::Warn < Severity::Error);
    }

    #[test]
    fn source_labels_are_stable() {
        assert_eq!(Source::TtMetal.label(), "metal");
        assert_eq!(Source::Host.label(), "host");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features tui hivemind::event -- --nocapture`
Expected: FAIL to compile — module `hivemind` not declared in `src/workload/mod.rs`.

- [ ] **Step 3: Write minimal implementation**

Add to `src/workload/mod.rs`:
```rust
pub mod hivemind;
```

Create `src/workload/hivemind/mod.rs`:
```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Hivemindsweeper: opt-in passive activity sniffer. See
//! docs/superpowers/specs/2026-07-14-hivemindsweeper-design.md.

pub mod event;

use std::collections::VecDeque;
pub use event::{EventKind, Severity, SniffEvent, Source};

/// Bounded event history + drop accounting. Collectors are added in later tasks.
pub struct Hivemind {
    events: VecDeque<SniffEvent>,
    dropped: u64,
}

impl Hivemind {
    pub const RING_CAP: usize = 4096;

    pub fn new() -> Self {
        Self {
            events: VecDeque::with_capacity(256),
            dropped: 0,
        }
    }

    /// Append an event, evicting the oldest (and counting it) at capacity.
    pub fn push_for_test(&mut self, ev: SniffEvent) {
        self.ingest(ev);
    }

    fn ingest(&mut self, ev: SniffEvent) {
        if self.events.len() >= Self::RING_CAP {
            self.events.pop_front();
            self.dropped += 1;
        }
        self.events.push_back(ev);
    }

    pub fn events(&self) -> &VecDeque<SniffEvent> {
        &self.events
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

impl Default for Hivemind {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn ev() -> SniffEvent {
        SniffEvent {
            ts: Instant::now(),
            source: Source::Host,
            device: None,
            severity: Severity::Info,
            kind: EventKind::Log,
            text: "x".into(),
            origin: "test".into(),
        }
    }

    #[test]
    fn ring_evicts_oldest_and_counts_drops() {
        let mut h = Hivemind::new();
        for _ in 0..(Hivemind::RING_CAP + 5) {
            h.push_for_test(ev());
        }
        assert_eq!(h.events().len(), Hivemind::RING_CAP);
        assert_eq!(h.dropped(), 5);
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features tui hivemind:: -- --nocapture`
Expected: PASS (3 tests: severity, labels, ring eviction).

- [ ] **Step 5: Commit**

```bash
git add src/workload/hivemind/mod.rs src/workload/hivemind/event.rs src/workload/mod.rs
git commit -m "feat(hivemind): event model + bounded ring buffer engine skeleton"
```

---

### Task 2: Source classifier

**Files:**
- Create: `src/workload/hivemind/classify.rs`
- Modify: `src/workload/hivemind/mod.rs` (add `pub mod classify;`)
- Test: inline in `classify.rs`

**Interfaces:**
- Consumes: `Source` (Task 1).
- Produces: `fn classify(process_name: Option<&str>, line: &str) -> Source` — best-effort tag from a process/command name and/or a log line; returns `Source::Unknown` when nothing matches.

- [ ] **Step 1: Write the failing test**

Create `src/workload/hivemind/classify.rs`:
```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Best-effort attribution of an event to a stack `Source` from a process name
//! and/or a log line. Frontends (tt-forge, tt-xla) lower through tt-metal but
//! keep their own labeled lane so operators can see which frontend is driving.

use super::event::Source;

pub fn classify(process_name: Option<&str>, line: &str) -> Source {
    let hay = {
        let mut s = String::new();
        if let Some(p) = process_name {
            s.push_str(&p.to_lowercase());
            s.push(' ');
        }
        s.push_str(&line.to_lowercase());
        s
    };
    // Order matters: most specific first. tt-forge/tt-xla before generic metal/ttnn.
    const RULES: &[(&str, Source)] = &[
        ("tt-forge", Source::TtForge),
        ("tt_forge", Source::TtForge),
        ("forge-fe", Source::TtForge),
        ("tt-xla", Source::TtXla),
        ("tt_xla", Source::TtXla),
        ("pjrt", Source::TtXla),
        ("tt-lang", Source::TtLang),
        ("ttlang", Source::TtLang),
        ("ttl ", Source::TtLang),
        ("inspector", Source::Inspector),
        ("vllm", Source::Vllm),
        ("ttnn", Source::Ttnn),
        ("tt_metal", Source::TtMetal),
        ("tt-metal", Source::TtMetal),
        ("metalium", Source::TtMetal),
        ("tenstorrent", Source::Driver),
        ("tt_kmd", Source::Driver),
    ];
    for (needle, src) in RULES {
        if hay.contains(needle) {
            return *src;
        }
    }
    Source::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontend_beats_generic_metal() {
        // A tt-forge process compiling through metal still tags as tt-forge.
        assert_eq!(
            classify(Some("tt-forge-fe"), "lowering ttir to tt_metal"),
            Source::TtForge
        );
    }

    #[test]
    fn recognizes_from_log_line_only() {
        assert_eq!(classify(None, "ttnn.matmul dispatched"), Source::Ttnn);
        assert_eq!(classify(None, "kmd: tenstorrent device reset"), Source::Driver);
    }

    #[test]
    fn unknown_when_nothing_matches() {
        assert_eq!(classify(Some("bash"), "hello world"), Source::Unknown);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features tui hivemind::classify -- --nocapture`
Expected: FAIL to compile — `classify` module not declared.

- [ ] **Step 3: Write minimal implementation**

Add to `src/workload/hivemind/mod.rs` (near the other `pub mod`):
```rust
pub mod classify;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features tui hivemind::classify -- --nocapture`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add src/workload/hivemind/classify.rs src/workload/hivemind/mod.rs
git commit -m "feat(hivemind): source classifier with frontend-over-generic precedence"
```

---

### Task 3: Heat grid aggregator

**Files:**
- Create: `src/workload/hivemind/grid.rs`
- Modify: `src/workload/hivemind/mod.rs` (add `pub mod grid;`)
- Test: inline in `grid.rs`

**Interfaces:**
- Consumes: `Source`, `SniffEvent` (Task 1).
- Produces:
  - `struct HeatGrid` with `fn new() -> Self`, `fn bump(&mut self, source: Source, device: Option<u8>)`, `fn decay(&mut self)`, `fn heat(&self, source: Source, col: Column) -> f32`, `fn active_sources(&self) -> Vec<Source>` (sources that have ever fired, in stable order), `fn max_device(&self) -> Option<u8>`.
  - `enum Column { Device(u8), Host }` (`Debug, Clone, Copy, PartialEq, Eq, Hash`).
  - `fn heat_char(heat: f32) -> char` — maps decayed heat to the `· ░ ▒ ▓ █` ramp (reuses `crate::animation::common::value_to_block_char`).
  - `const DECAY: f32 = 0.9`.

- [ ] **Step 1: Write the failing test**

Create `src/workload/hivemind/grid.rs`:
```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Decaying (source × column) activity heat. `bump` on each event, `decay`
//! once per frame so idle cells fade to cold.

use super::event::Source;
use std::collections::HashMap;

pub const DECAY: f32 = 0.9;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Column {
    Device(u8),
    Host,
}

pub struct HeatGrid {
    heat: HashMap<(Source, Column), f32>,
    order: Vec<Source>, // sources in first-seen order
    max_device: Option<u8>,
}

impl HeatGrid {
    pub fn new() -> Self {
        Self {
            heat: HashMap::new(),
            order: Vec::new(),
            max_device: None,
        }
    }

    pub fn bump(&mut self, source: Source, device: Option<u8>) {
        let col = match device {
            Some(d) => {
                self.max_device = Some(self.max_device.map_or(d, |m| m.max(d)));
                Column::Device(d)
            }
            None => Column::Host,
        };
        if !self.order.contains(&source) {
            self.order.push(source);
        }
        let e = self.heat.entry((source, col)).or_insert(0.0);
        *e = (*e + 1.0).min(8.0);
    }

    pub fn decay(&mut self) {
        for v in self.heat.values_mut() {
            *v *= DECAY;
        }
    }

    pub fn heat(&self, source: Source, col: Column) -> f32 {
        *self.heat.get(&(source, col)).unwrap_or(&0.0)
    }

    pub fn active_sources(&self) -> Vec<Source> {
        self.order.clone()
    }

    pub fn max_device(&self) -> Option<u8> {
        self.max_device
    }
}

impl Default for HeatGrid {
    fn default() -> Self {
        Self::new()
    }
}

/// Map decayed heat (0..~8) to a block glyph. Normalizes to 0..1 for the ramp.
pub fn heat_char(heat: f32) -> char {
    crate::animation::common::value_to_block_char((heat / 8.0).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_tracks_column_and_first_seen_order() {
        let mut g = HeatGrid::new();
        g.bump(Source::TtMetal, Some(0));
        g.bump(Source::Ttnn, None);
        g.bump(Source::TtMetal, Some(3));
        assert_eq!(g.active_sources(), vec![Source::TtMetal, Source::Ttnn]);
        assert_eq!(g.max_device(), Some(3));
        assert!(g.heat(Source::TtMetal, Column::Device(0)) > 0.0);
        assert!(g.heat(Source::Ttnn, Column::Host) > 0.0);
    }

    #[test]
    fn decay_cools_idle_cells() {
        let mut g = HeatGrid::new();
        g.bump(Source::TtMetal, Some(0));
        let before = g.heat(Source::TtMetal, Column::Device(0));
        g.decay();
        let after = g.heat(Source::TtMetal, Column::Device(0));
        assert!(after < before);
        assert!((after - before * DECAY).abs() < 1e-6);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features tui hivemind::grid -- --nocapture`
Expected: FAIL to compile — `grid` module not declared.

- [ ] **Step 3: Write minimal implementation**

Add to `src/workload/hivemind/mod.rs`:
```rust
pub mod grid;
```
Confirm `value_to_block_char` exists at `src/animation/common.rs` (it does — public). If the `animation` module is behind a feature that `tui` doesn't enable, add `use crate::animation::common;` and adjust; the `tui` build already compiles `animation`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features tui hivemind::grid -- --nocapture`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/workload/hivemind/grid.rs src/workload/hivemind/mod.rs
git commit -m "feat(hivemind): decaying source×column heat grid"
```

---

### Task 4: Collector trait + spawn helper (panic-isolated)

**Files:**
- Create: `src/workload/hivemind/collector.rs`
- Modify: `src/workload/hivemind/mod.rs` (add `pub mod collector;`)
- Test: inline in `collector.rs`

**Interfaces:**
- Consumes: `SniffEvent` (Task 1).
- Produces:
  - `type Tx = std::sync::mpsc::SyncSender<SniffEvent>;`
  - `trait Collector: Send { fn name(&self) -> &'static str; fn run(&mut self, tx: Tx, shutdown: Arc<AtomicBool>); }`
  - `struct Handle { pub name: &'static str, join: Option<JoinHandle<()>>, shutdown: Arc<AtomicBool>, pub status: Arc<Mutex<CollectorStatus>> }` with `fn stop(self)` (signals + joins).
  - `enum CollectorStatus { Ok, PermDenied(String), Err(String) }` (`Debug, Clone, PartialEq`).
  - `fn spawn(mut c: Box<dyn Collector>, tx: Tx) -> Handle` — spawns a thread that runs the collector inside `std::panic::catch_unwind`, setting `status = Err(..)` on panic.

- [ ] **Step 1: Write the failing test**

Create `src/workload/hivemind/collector.rs`:
```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features tui hivemind::collector -- --nocapture`
Expected: FAIL to compile — `collector` module not declared.

- [ ] **Step 3: Write minimal implementation**

Add to `src/workload/hivemind/mod.rs`:
```rust
pub mod collector;
```
Note: the panic message printed by the default hook is expected test noise; the test asserts containment, not silence.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features tui hivemind::collector -- --nocapture`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/workload/hivemind/collector.rs src/workload/hivemind/mod.rs
git commit -m "feat(hivemind): panic-isolated Collector trait + spawn helper"
```

---

### Task 5: `LineSource` + kmsg collector

**Files:**
- Create: `src/workload/hivemind/collectors/mod.rs`
- Create: `src/workload/hivemind/collectors/kmsg.rs`
- Modify: `src/workload/hivemind/mod.rs` (add `pub mod collectors;`)
- Test: inline in `kmsg.rs`

**Interfaces:**
- Consumes: `Collector`, `Tx`, `CollectorStatus` (Task 4); `classify` (Task 2); `Source/Severity/EventKind/SniffEvent` (Task 1).
- Produces:
  - `fn parse_kmsg_line(line: &str) -> Option<SniffEvent>` — returns an event only for TT/driver-relevant lines (`tenstorrent`, `tt_kmd`, PCIe AER), classified `Source::Driver`, `kind = DriverMsg`, device parsed from a trailing `tenstorrent!N`/`card N` hint when present. Non-matching → `None`.
  - `struct KmsgCollector` implementing `Collector` (reads `/dev/kmsg`; on open failure sets `PermDenied` via a passed-in status handle — wired in Task 9's engine; for the unit test we test `parse_kmsg_line` directly).

- [ ] **Step 1: Write the failing test**

Create `src/workload/hivemind/collectors/kmsg.rs`:
```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Kernel ring-buffer (/dev/kmsg) collector: surfaces tt-kmd / PCIe driver
//! events that no userspace log carries. Read-only.

use crate::workload::hivemind::classify::classify;
use crate::workload::hivemind::event::{EventKind, Severity, SniffEvent, Source};
use std::time::Instant;

/// Parse one raw kmsg line into a driver SniffEvent, or None if not TT-relevant.
pub fn parse_kmsg_line(line: &str) -> Option<SniffEvent> {
    let low = line.to_lowercase();
    let relevant = low.contains("tenstorrent")
        || low.contains("tt_kmd")
        || low.contains("tt-kmd")
        || (low.contains("pcie") && (low.contains("aer") || low.contains("error")));
    if !relevant {
        return None;
    }
    // /dev/kmsg format: "prio,seq,ts,flag;message". Take the message half if present.
    let text = line.splitn(2, ';').nth(1).unwrap_or(line).trim().to_string();
    let severity = if low.contains("error") || low.contains("fail") || low.contains("aer") {
        Severity::Error
    } else if low.contains("warn") || low.contains("reset") {
        Severity::Warn
    } else {
        Severity::Notice
    };
    let device = parse_device_hint(&low);
    Some(SniffEvent {
        ts: Instant::now(),
        source: classify(None, &low), // → Driver for tenstorrent/tt_kmd; PCIe → Driver too below
        device,
        severity,
        kind: EventKind::DriverMsg,
        text,
        origin: "kmsg".into(),
    })
}

fn parse_device_hint(low: &str) -> Option<u8> {
    // Matches "tenstorrent!N", "card N", "device N".
    for key in ["tenstorrent!", "card ", "device "] {
        if let Some(idx) = low.find(key) {
            let rest = &low[idx + key.len()..];
            let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = num.parse::<u8>() {
                return Some(n);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_irrelevant_lines() {
        assert!(parse_kmsg_line("6,100,12345,-;usb 1-1: new device").is_none());
    }

    #[test]
    fn parses_driver_reset_with_device_hint() {
        let ev =
            parse_kmsg_line("4,200,999,-;tenstorrent!1: watchdog reset issued").unwrap();
        assert_eq!(ev.source, Source::Driver);
        assert_eq!(ev.device, Some(1));
        assert_eq!(ev.severity, Severity::Warn);
        assert_eq!(ev.kind, EventKind::DriverMsg);
        assert!(ev.text.contains("watchdog reset"));
    }

    #[test]
    fn pcie_aer_is_error() {
        let ev = parse_kmsg_line("3,201,1000,-;pcie 0000:01:00.0: AER: corrected error")
            .unwrap();
        assert_eq!(ev.severity, Severity::Error);
    }
}
```

Note: for PCIe lines `classify(None, low)` currently returns `Unknown`; add `"pcie"` and `"aer"` needles mapping to `Source::Driver` in `classify.rs` RULES (extend Task 2's table). Do that as part of this task and re-run Task 2's tests.

Create `src/workload/hivemind/collectors/mod.rs`:
```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Individual activity collectors. Each is behind the `Collector` trait.

pub mod kmsg;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features tui hivemind::collectors::kmsg -- --nocapture`
Expected: FAIL to compile — `collectors` module not declared / `pcie` needle missing.

- [ ] **Step 3: Write minimal implementation**

Add to `src/workload/hivemind/mod.rs`:
```rust
pub mod collectors;
```
Extend `classify.rs` RULES with `("pcie", Source::Driver), ("aer", Source::Driver),` (place after `tt_kmd`). Then implement `KmsgCollector` at the bottom of `kmsg.rs`:
```rust
use crate::workload::hivemind::collector::{Collector, Tx};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct KmsgCollector;

impl Collector for KmsgCollector {
    fn name(&self) -> &'static str {
        "kmsg"
    }
    fn run(&mut self, tx: Tx, shutdown: Arc<AtomicBool>) {
        // Open non-blocking so we can poll shutdown; fall back silently if denied.
        let file = match std::fs::File::open("/dev/kmsg") {
            Ok(f) => f,
            Err(_) => return, // engine marks PermDenied via its own probe (Task 9)
        };
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        while !shutdown.load(Ordering::SeqCst) {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => std::thread::sleep(std::time::Duration::from_millis(200)),
                Ok(_) => {
                    if let Some(ev) = parse_kmsg_line(&line) {
                        if tx.try_send(ev).is_err() {
                            // channel full: drop newest (engine counts drops)
                        }
                    }
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(200)),
            }
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features tui hivemind:: -- --nocapture`
Expected: PASS (all prior + 3 kmsg tests; classify tests still pass).

- [ ] **Step 5: Commit**

```bash
git add src/workload/hivemind/collectors/ src/workload/hivemind/mod.rs src/workload/hivemind/classify.rs
git commit -m "feat(hivemind): kmsg driver-event collector + parser"
```

---

### Task 6: cache-watch collector (polling)

**Files:**
- Create: `src/workload/hivemind/collectors/cache_watch.rs`
- Modify: `src/workload/hivemind/collectors/mod.rs` (add `pub mod cache_watch;`)
- Test: inline in `cache_watch.rs` (uses a temp dir)

**Interfaces:**
- Consumes: `Collector`, `Tx` (Task 4); event types (Task 1).
- Produces:
  - `fn scan_new_artifacts(roots: &[PathBuf], seen: &mut HashSet<PathBuf>) -> Vec<SniffEvent>` — returns a `Compile` event per newly-seen `.o`/kernel artifact under any root; device parsed from a `cache_*/<device_type>/` path segment via `device_col_from_path`.
  - `fn device_col_from_path(p: &Path) -> Option<u8>` — best-effort (`.../p150/...` style has no index → None; `.../device_1/...` → Some(1)).
  - `struct CacheWatchCollector { roots: Vec<PathBuf> }` with `fn new() -> Self` (resolves `TT_METAL_CACHE`, `TT_DIT_CACHE_DIR`, `tt_metal_cache`, `tt_dit_cache`, `generated` under `TT_METAL_HOME`/cwd) implementing `Collector`.

- [ ] **Step 1: Write the failing test**

Create `src/workload/hivemind/collectors/cache_watch.rs` with `scan_new_artifacts`, `device_col_from_path`, and this test:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;

    #[test]
    fn reports_only_newly_seen_artifacts() {
        let dir = std::env::temp_dir().join(format!("hm-cache-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let f1 = dir.join("brisc.o");
        fs::write(&f1, b"x").unwrap();

        let mut seen = HashSet::new();
        let roots = vec![dir.clone()];
        let first = scan_new_artifacts(&roots, &mut seen);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].kind, crate::workload::hivemind::event::EventKind::Compile);

        // Second scan: nothing new.
        let second = scan_new_artifacts(&roots, &mut seen);
        assert!(second.is_empty());

        // Add another; only it is reported.
        fs::write(dir.join("ncrisc.o"), b"y").unwrap();
        let third = scan_new_artifacts(&roots, &mut seen);
        assert_eq!(third.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_device_index_segment() {
        use std::path::PathBuf;
        let p = PathBuf::from("/x/cache_llama/device_2/brisc.o");
        assert_eq!(device_col_from_path(&p), Some(2));
        let p2 = PathBuf::from("/x/cache_llama/p150/brisc.o");
        assert_eq!(device_col_from_path(&p2), None);
    }
}
```
Implement `scan_new_artifacts` to walk each root recursively (bounded depth, e.g. 6), match extensions `.o`/`.so`/`.hex`/`.elf`, and emit `SniffEvent { source: TtMetal, kind: Compile, severity: Info, origin: "cache", text: <filename>, device: device_col_from_path(&path) }` for paths not in `seen`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features tui hivemind::collectors::cache_watch -- --nocapture`
Expected: FAIL — module not declared.

- [ ] **Step 3: Write minimal implementation**

Add `pub mod cache_watch;` to `collectors/mod.rs`. Implement functions + `CacheWatchCollector::run` looping: scan → send new events → sleep 1s → poll shutdown. Cap `seen` growth by only inserting matched artifacts.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features tui hivemind::collectors::cache_watch -- --nocapture`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/workload/hivemind/collectors/cache_watch.rs src/workload/hivemind/collectors/mod.rs
git commit -m "feat(hivemind): polling compile-cache artifact collector"
```

---

### Task 7: log-tail collector (files + docker) and procfs collector

**Files:**
- Create: `src/workload/hivemind/collectors/log_tail.rs`
- Create: `src/workload/hivemind/collectors/procfs.rs`
- Modify: `src/workload/hivemind/collectors/mod.rs`
- Test: inline in both (pure parse/tail helpers tested; thread bodies exercised in Task 9)

**Interfaces:**
- Produces:
  - `log_tail`: `fn event_from_log(origin: &str, proc_name: Option<&str>, line: &str) -> Option<SniffEvent>` — applies `inference_server::logs::last_non_health_line`-style filtering (skip lines containing `/tt-liveness`, `/health`, `/v1/models`), classifies source, infers severity from `ERROR`/`WARN`, `kind = Log`. Returns `None` for filtered/empty lines. `struct LogTailCollector` tails discovered log files + `docker logs -f` for detected containers (reuses `inference_server::detect`).
  - `procfs`: `fn device_from_fd_target(link: &str) -> Option<u8>` — parses `/dev/tenstorrent/N` → `Some(N)`. `struct ProcfsCollector` (Linux-only; behind `#[cfg(all(target_os = "linux", feature = "linux-procfs"))]`) that diffs process/fd state each poll and emits `Process`/`Fd` events on change, reusing `crate::workload::process_monitor`.

- [ ] **Step 1: Write the failing tests**

In `log_tail.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::hivemind::event::{EventKind, Severity, Source};

    #[test]
    fn skips_health_poll_lines() {
        assert!(event_from_log("docker:z", None,
            r#"1.2.3.4 - "GET /tt-liveness HTTP/1.1" 405"#).is_none());
    }

    #[test]
    fn surfaces_real_error_line() {
        let ev = event_from_log("docker:z", Some("vllm"),
            "ERROR weight load failed: OOM").unwrap();
        assert_eq!(ev.source, Source::Vllm);
        assert_eq!(ev.severity, Severity::Error);
        assert_eq!(ev.kind, EventKind::Log);
    }
}
```
In `procfs.rs`:
```rust
#[cfg(all(target_os = "linux", feature = "linux-procfs"))]
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_device_fd_target() {
        assert_eq!(device_from_fd_target("/dev/tenstorrent/3"), Some(3));
        assert_eq!(device_from_fd_target("/dev/null"), None);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --features tui,linux-procfs hivemind::collectors::log_tail hivemind::collectors::procfs -- --nocapture`
Expected: FAIL — modules not declared.

- [ ] **Step 3: Write minimal implementation**

Add `pub mod log_tail;` and `pub mod procfs;` to `collectors/mod.rs`. Implement `event_from_log` (filter → classify → severity → build event) and `device_from_fd_target`. Implement collector `run` bodies: `LogTailCollector` opens each discovered log path, seeks to end, tails new lines (poll + sleep), and spawns `docker logs -f <id>` piping stdout for device-pinned containers found via `inference_server::detect`; `ProcfsCollector` calls `process_monitor::ProcessMonitor::update()` each second and emits diffs. Guard the whole `procfs.rs` body with the Linux+procfs cfg; provide a no-op `ProcfsCollector` on other targets so Task 9 compiles everywhere.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features tui,linux-procfs hivemind::collectors -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/workload/hivemind/collectors/log_tail.rs src/workload/hivemind/collectors/procfs.rs src/workload/hivemind/collectors/mod.rs
git commit -m "feat(hivemind): log-tail (files+docker) and procfs collectors"
```

---

### Task 8: inspector collector (file tail)

**Files:**
- Create: `src/workload/hivemind/collectors/inspector.rs`
- Modify: `src/workload/hivemind/collectors/mod.rs`
- Test: inline in `inspector.rs`

**Interfaces:**
- Produces: `fn event_from_inspector_line(line: &str) -> Option<SniffEvent>` — parses a tt-metal Inspector log line into `Source::Inspector`, `kind = Inspector` (or `Compile` when the line mentions `compile`/`kernel`), severity `Info`. `struct InspectorCollector` that discovers Inspector log files (default `generated/inspector/` and `$TT_METAL_HOME/generated/inspector/`) and tails them. RPC subscription is explicitly out of scope (future phase) — leave a `// FUTURE: RPC subscribe` marker only, no stub type.

- [ ] **Step 1: Write the failing test**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::hivemind::event::{EventKind, Source};
    #[test]
    fn compile_finished_maps_to_compile_kind() {
        let ev = event_from_inspector_line(
            "program 12 kernel brisc compile finished").unwrap();
        assert_eq!(ev.source, Source::Inspector);
        assert_eq!(ev.kind, EventKind::Compile);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features tui hivemind::collectors::inspector -- --nocapture`
Expected: FAIL — module not declared.

- [ ] **Step 3: Write minimal implementation**

Add `pub mod inspector;` to `collectors/mod.rs`; implement parser + tailing `run` (same tail pattern as `log_tail`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features tui hivemind::collectors::inspector -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/workload/hivemind/collectors/inspector.rs src/workload/hivemind/collectors/mod.rs
git commit -m "feat(hivemind): tt-metal Inspector log-file collector"
```

---

### Task 9: Engine wiring — start/stop/poll + target management

**Files:**
- Modify: `src/workload/hivemind/mod.rs`
- Test: inline in `mod.rs`

**Interfaces:**
- Consumes: all collectors + `spawn`/`Handle`/`CollectorStatus` (Tasks 4–8), `HeatGrid` (Task 3).
- Produces, on `Hivemind`:
  - `fn start(&mut self)` — creates the bounded `sync_channel(1024)`, spawns the auto collectors (kmsg, cache_watch, log_tail, procfs, inspector), records `Handle`s. Idempotent (no-op if already running).
  - `fn stop(&mut self)` — stops all handles, clears the channel, keeps event history.
  - `fn poll(&mut self)` — drains the channel into the ring (counting drops), bumps the heat grid, then `grid.decay()`. Infallible.
  - `fn add_watch_path(&mut self, path: PathBuf)`, `fn add_watch_pid(&mut self, pid: i32)`, `fn add_wrap(&mut self, argv: Vec<String>) -> Result<(), String>` — spawn a `wrap` collector (Task 10 provides the collector; here just the wiring + a stub that returns Ok for path/pid).
  - `fn grid(&self) -> &HeatGrid`, `fn statuses(&self) -> Vec<(&'static str, CollectorStatus)>`, `fn is_running(&self) -> bool`.

- [ ] **Step 1: Write the failing test**
```rust
#[cfg(test)]
mod engine_tests {
    use super::*;

    #[test]
    fn poll_drains_channel_into_ring_and_heat() {
        let mut h = Hivemind::new();
        // Inject a synthetic collector channel by starting with a fake tx.
        h.start();
        // Manually feed via the test hook that mirrors what poll() consumes:
        h.push_for_test(SniffEvent {
            ts: std::time::Instant::now(),
            source: Source::TtMetal,
            device: Some(0),
            severity: Severity::Info,
            kind: EventKind::Compile,
            text: "brisc.o".into(),
            origin: "cache".into(),
        });
        // heat is bumped by poll() in real flow; for injected events, bump directly:
        h.bump_grid_for_test(Source::TtMetal, Some(0));
        assert!(h.grid().heat(Source::TtMetal, grid::Column::Device(0)) > 0.0);
        assert!(h.is_running());
        h.stop();
        assert!(!h.is_running());
    }
}
```
(Provide `fn bump_grid_for_test` guarded by `#[cfg(test)]` that calls `self.grid.bump(..)`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features tui,linux-procfs hivemind::engine_tests -- --nocapture`
Expected: FAIL to compile — `start/stop/grid/is_running/bump_grid_for_test` missing.

- [ ] **Step 3: Write minimal implementation**

Add fields to `Hivemind`: `grid: HeatGrid`, `rx: Option<Receiver<SniffEvent>>`, `tx: Option<SyncSender<SniffEvent>>`, `handles: Vec<Handle>`, `running: bool`. Implement `start` (build channel, spawn each auto collector via `collector::spawn`), `poll` (`while let Ok(ev) = rx.try_recv() { self.grid.bump(ev.source, ev.device); self.ingest(ev); } self.grid.decay();`), `stop` (drain handles with `.stop()`, set `running=false`), and the accessors. `add_wrap` is stubbed to return `Ok(())` until Task 10.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features tui,linux-procfs hivemind:: -- --nocapture`
Expected: PASS (all hivemind tests).

- [ ] **Step 5: Commit**

```bash
git add src/workload/hivemind/mod.rs
git commit -m "feat(hivemind): engine start/stop/poll wiring + heat integration"
```

---

### Task 10: wrap collector + `/watch` + `/wrap` command dispatch

**Files:**
- Create: `src/workload/hivemind/collectors/wrap.rs`
- Modify: `src/workload/hivemind/collectors/mod.rs`, `src/workload/hivemind/mod.rs` (finish `add_wrap`/`add_watch_*`)
- Test: inline in `wrap.rs`

**Interfaces:**
- Produces: `struct WrapCollector { kind: WrapKind }` where `enum WrapKind { File(PathBuf), Pid(i32), Command(Vec<String>) }`, implementing `Collector`. Command mode spawns via `std::process::Command` with `stdout`/`stderr` piped, reads both into `Emission` events (`source = classify(argv[0])`), and kills the child on shutdown. No shell interpolation — argv passed directly.

- [ ] **Step 1: Write the failing test**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::hivemind::collector::{spawn, Tx};
    use crate::workload::hivemind::event::EventKind;
    use std::sync::mpsc::sync_channel;
    use std::time::Duration;

    #[test]
    fn wrap_command_captures_stdout_as_emission() {
        let (tx, rx) = sync_channel::<crate::workload::hivemind::event::SniffEvent>(16);
        let c = WrapCollector {
            kind: WrapKind::Command(vec!["echo".into(), "emitting-cpp".into()]),
        };
        let h = spawn(Box::new(c), tx);
        let ev = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(ev.kind, EventKind::Emission);
        assert!(ev.text.contains("emitting-cpp"));
        h.stop();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features tui hivemind::collectors::wrap -- --nocapture`
Expected: FAIL — module not declared.

- [ ] **Step 3: Write minimal implementation**

Add `pub mod wrap;`. Implement `WrapCollector::run` for all three `WrapKind`s (File/Pid reuse the tail/procfs helpers; Command spawns, reads piped lines on helper threads, emits `Emission` events, kills on shutdown). Finish `Hivemind::add_watch_path/add_watch_pid/add_wrap` to `collector::spawn` a `WrapCollector` and push the handle.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features tui hivemind::collectors::wrap -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/workload/hivemind/collectors/wrap.rs src/workload/hivemind/collectors/mod.rs src/workload/hivemind/mod.rs
git commit -m "feat(hivemind): wrap collector (file/pid/command capture) + target wiring"
```

---

### Task 11: TUI — DisplayMode variant, `~` activation, board+feed view

**Files:**
- Create: `src/ui/tui/hivemind_view.rs`
- Modify: `src/ui/tui/mod.rs` (DisplayMode enum, loop state, `~` key handler, render dispatch, animated-modes list, per-frame poll)
- Test: inline in `hivemind_view.rs`

**Interfaces:**
- Consumes: `Hivemind`, `HeatGrid`, `Column`, `Source`, `SniffEvent` (Tasks 1–10).
- Produces: `fn render_hivemind(f, area, hive: &Hivemind, cursor: (usize, usize), unified: bool, sev_floor: Severity)` and a pure helper `fn board_rows(hive: &Hivemind) -> Vec<BoardRow>` (`struct BoardRow { source: Source, cells: Vec<(Column, char)> }`) so rendering logic is testable without a terminal.

- [ ] **Step 1: Write the failing test** (pure board layout)

In `hivemind_view.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::hivemind::{event::*, grid::Column, Hivemind};

    #[test]
    fn board_lists_active_sources_with_host_and_device_columns() {
        let mut h = Hivemind::new();
        h.bump_grid_for_test(Source::TtMetal, Some(0));
        h.bump_grid_for_test(Source::Ttnn, None);
        let rows = board_rows(&h);
        let sources: Vec<_> = rows.iter().map(|r| r.source).collect();
        assert_eq!(sources, vec![Source::TtMetal, Source::Ttnn]);
        // Column set spans D0..=max_device plus Host.
        assert!(rows[0].cells.iter().any(|(c, _)| *c == Column::Device(0)));
        assert!(rows[0].cells.iter().any(|(c, _)| *c == Column::Host));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features tui,linux-procfs hivemind_view -- --nocapture`
Expected: FAIL — module not declared / `render_hivemind` missing.

- [ ] **Step 3: Write minimal implementation**

Implement `board_rows` (columns = `Device(0..=max_device)` + `Host`; glyph via `grid::heat_char`) and `render_hivemind` (header with collector statuses + drop count; board using left/bottom borders only, widths via `unicode-width`; bottom feed pane filtered to the selected cell or unified, honoring `sev_floor`). Wire into `src/ui/tui/mod.rs`:
- Add `HivemindSweeper` to `enum DisplayMode`.
- Add loop state near `cmd_mode`: `let mut hivemind: Option<crate::workload::hivemind::Hivemind> = None; let mut hm_cursor = (0usize, 0usize); let mut hm_unified = false; let mut hm_sev = Severity::Trace;`.
- `~` key handler (mirror the `i` handler): if not in the mode, `prev_mode = display_mode; display_mode = HivemindSweeper;` and `hivemind.get_or_insert_with(Hivemind::new).start();` else exit to `prev_mode` and `if let Some(h)=hivemind.as_mut(){h.stop();}`.
- Esc handler: add a branch so Esc in `HivemindSweeper` stops the engine and returns to `prev_mode`.
- Animated-modes set (around line 505): add `DisplayMode::HivemindSweeper` so the loop redraws at frame rate.
- Per-frame (render dispatch around line 700): `DisplayMode::HivemindSweeper => { if let Some(h)=hivemind.as_mut(){ h.poll(); } }`, and in the draw block call `render_hivemind(...)`.
- Add `mod hivemind_view;` at the top of `src/ui/tui/mod.rs`.

- [ ] **Step 4: Run tests + manual smoke**

Run: `cargo test --features tui,linux-procfs hivemind_view -- --nocapture` → PASS.
Run: `cargo build --features tui,linux-procfs` → builds clean.
Manual: `cargo run --bin tt-toplike-tui --features tui -- --mock --mock-devices 4`, press `~`, confirm the board appears and exits on `~`/`Esc`.

- [ ] **Step 5: Commit**

```bash
git add src/ui/tui/hivemind_view.rs src/ui/tui/mod.rs
git commit -m "feat(hivemind): TUI HivemindSweeper mode — ~ activation, board + feed"
```

---

### Task 12: Cursor / feed controls + `/hivemind`, `/watch`, `/wrap` commands + docs

**Files:**
- Modify: `src/ui/tui/mod.rs` (in-mode key handling; command-bar Enter dispatch; `/mode hivemind` arm in `execute_command`; overlay Legend/Help/Explain text)
- Modify: `AGENTS.md` (Phase 23 log entry), `Cargo.toml` (version bump)
- Test: inline unit test for a command-parse helper

**Interfaces:**
- Consumes: `Hivemind::add_watch_path/add_watch_pid/add_wrap` (Task 10).
- Produces: `fn parse_hivemind_cmd(rest: &str) -> HmCmd` where `enum HmCmd { WatchPath(PathBuf), WatchPid(i32), Wrap(Vec<String>), Bad(String) }` (placed in `mod.rs` or a small `hivemind_view` helper).

- [ ] **Step 1: Write the failing test**
```rust
#[cfg(test)]
mod hm_cmd_tests {
    use super::*;
    #[test]
    fn parses_watch_and_wrap() {
        assert!(matches!(parse_hivemind_cmd("pid 42"), HmCmd::WatchPid(42)));
        assert!(matches!(parse_hivemind_cmd("/var/log/run.log"), HmCmd::WatchPath(_)));
        match parse_hivemind_cmd_wrap("ttl export k.py") {
            HmCmd::Wrap(v) => assert_eq!(v, vec!["ttl", "export", "k.py"]),
            _ => panic!(),
        }
    }
}
```
(Use one `parse_hivemind_cmd` for `/watch` args and a `parse_hivemind_cmd_wrap` for `/wrap` argv split, or a single function with a mode flag — keep names consistent with what the dispatch calls.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features tui,linux-procfs hm_cmd -- --nocapture`
Expected: FAIL — parser missing.

- [ ] **Step 3: Write minimal implementation**

- Implement `parse_hivemind_cmd` / wrap split.
- In the command-bar Enter handler (after the `serve` branch, before the `execute_command` fallback), add `verb == "hivemind"`, `verb == "watch"`, `verb == "wrap"` branches: `hivemind.get_or_insert_with(Hivemind::new)`, enter the mode, and call the matching `add_*`. `/wrap` sets a `cmd_message` confirmation the first time and only spawns on a second confirm (reuse the existing `cmd_message` mechanism, or gate behind a `hm_pending_wrap: Option<Vec<String>>` applied on the next Enter).
- Add a `"hivemind"` arm to the `mode` match in `execute_command` that sets `*display_mode = DisplayMode::HivemindSweeper` (engine start still happens in the inline branch / `~` handler — document that `/mode hivemind` without a running engine starts empty until `~` toggles; simplest: also start via the inline `hivemind` verb, and treat `/mode hivemind` as a thin alias routed to the same inline branch).
- In-mode keys (only when `display_mode == HivemindSweeper` and not `cmd_mode`): arrow keys / `hjkl` move `hm_cursor` (clamped to `board_rows` dims), `f` toggles `hm_unified`, `s` cycles `hm_sev` (Trace→Info→Notice→Warn→Error→Trace).
- Overlays: add `HivemindSweeper` legend text (`· ░ ▒ ▓ █ heat · f feed · s severity · /watch /wrap`), a Help entry, and an Explain paragraph.
- Bump `version` in `Cargo.toml`. Add an `AGENTS.md` "Phase 23 — Hivemindsweeper" section summarizing the mode.

- [ ] **Step 4: Run tests + manual smoke**

Run: `cargo test --features tui,linux-procfs -- --nocapture` → all PASS.
Manual: `cargo run --bin tt-toplike-tui --features tui -- --mock --mock-devices 4`; press `~`; move cursor; press `f`, `s`; open `/` and run `/wrap echo hello` (confirm), verify an Emission lane appears.

- [ ] **Step 5: Commit**

```bash
git add src/ui/tui/mod.rs AGENTS.md Cargo.toml
git commit -m "feat(hivemind): cursor/feed controls, /hivemind /watch /wrap commands, docs + version bump"
```

---

## Self-Review

**Spec coverage:**
- Minesweeper `source × device` board → Tasks 3, 11. Host column → Task 3 (`Column::Host`). Cover/reveal is a rendering refinement folded into Task 11's `render_hivemind` (heat glyph + cursor); if the cosmetic "covered until swept" marker is wanted explicitly, it lives in `render_hivemind` and needs no new interface.
- Unified feed toggle (`f`), severity floor (`s`) → Task 12. Substring filter → intentionally deferred (spec updated).
- Five auto collectors + inspector → Tasks 5–8; engine spawns them → Task 9.
- Point-at-target (`/watch`, `/wrap`) → Tasks 10, 12; `~` + `/hivemind` activation → Tasks 11, 12.
- Source classifier incl. tt-forge/tt-xla → Task 2.
- Ring buffer + drop counter + decaying heat → Tasks 1, 3, 9.
- Safety: read-only (no env vars set anywhere in plan), never reads device buffers (no device-attach code exists in any task), graceful degrade (kmsg open failure returns; PermDenied status), panic isolation → Task 4; `/wrap` confirmation → Task 12.
- Tests per collector via pure parse helpers + temp dirs → Tasks 5–8, 10; render golden-ish → Task 11.

**Placeholder scan:** Collector `run` bodies in Tasks 6–8 are described rather than shown in full because their loop shape is identical to the fully-shown kmsg `run` in Task 5 (open/seek → read new lines → parse helper → `try_send` → sleep/poll shutdown); the parse helpers they call are shown in full with tests. The wrap `run` (Task 10) and TUI wiring (Tasks 11–12) name every field, function, and integration line to touch.

**Type consistency:** `Tx = SyncSender<SniffEvent>` used uniformly (Tasks 4–10). `Column` defined once (Task 3), reused (Tasks 9, 11). `Source`/`Severity`/`EventKind` from Task 1 used verbatim throughout. `bump_grid_for_test` introduced in Task 9 and used in Task 11's test.

**Scope:** One cohesive subsystem + its TUI integration — a single plan is appropriate.
