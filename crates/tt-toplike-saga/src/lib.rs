//! Saga: a generative quest view built from the signal bus.
//!
//! This crate is a pure, deterministic core. It turns an ordered stream of bus
//! records into a world state and saga events. The same ordered stream of
//! record contents gives the same saga: the core reads no sequence number and
//! no timestamp for any decision, and its clock is the count of pacemaker
//! sweeps it has seen.
//!
//! Three fences keep it that way:
//!
//! * `Cargo.toml` lists only `tt-toplike-bus` (and `serde` if needed).
//! * `clippy.toml` bans `Instant::now`, `SystemTime::now`, `std::env::var`,
//!   `HashMap` and `HashSet`. Use `BTreeMap` and `BTreeSet` so iteration order
//!   is fixed.
//! * `#![forbid(unsafe_code)]` below.
//!
//! Inside the core, arithmetic is limited to `+ - * /`, comparisons, `sqrt`
//! and integer operations. `sin`, `cos`, `exp`, `powf` and `ln` can differ
//! between platforms and are not used.
//!
//! Modules are added by the tasks that implement them; each adds its own
//! `pub mod` line here.

#![forbid(unsafe_code)]

pub mod observe;
pub mod rng;
