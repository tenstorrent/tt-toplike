//! Saga, the quest view, on the root crate's side.
//!
//! The story itself is told by the pure core in the `tt_toplike_saga` crate:
//! it turns an ordered stream of bus records into a world, a hero and a
//! chronicle, and the same stream always gives the same saga (spec 3.5).
//! This module holds what needs the root crate:
//!
//! * [`config`] turns the `[saga]` config section into the core's
//!   [`tt_toplike_saga::SagaConfig`] ([`saga_config`]).
//! * [`driver`] runs the core on a background thread fed from the bus and
//!   publishes its state for the view ([`SagaDriver`]).

pub mod config;
pub mod driver;

pub use config::saga_config;
pub use driver::{label_for, Published, SagaDriver};
