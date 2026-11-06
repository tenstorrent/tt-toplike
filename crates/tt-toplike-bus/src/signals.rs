//! Signal names that more than one crate agrees on.
//!
//! Sources emit these as [`crate::Sample`]s on a model entity
//! ([`crate::EntityKey::model`]). The [`crate::World`] reads them into a
//! [`crate::ModelView`]. The strings are part of the log format: renaming one
//! breaks replay of old logs, so add new names and leave these alone.

/// Display name of the model (text). Defaults to the model key.
pub const MODEL_LABEL: &str = "model.label";
/// Lifecycle phase (text): `down`, `compiling`, `loading`, `ready` or `alarm`.
/// A phase sample and a model event both set the phase; the later one wins.
pub const MODEL_PHASE: &str = "model.phase";
/// Generated tokens per second (any number).
pub const MODEL_TOKENS_PER_S: &str = "model.tokens_per_s";
/// Requests being served right now (unsigned integer).
pub const MODEL_RUNNING: &str = "model.running";
/// Requests waiting in the queue (unsigned integer).
pub const MODEL_WAITING: &str = "model.waiting";
/// KV cache fill as a fraction from 0.0 to 1.0 (float).
pub const MODEL_KV_CACHE: &str = "model.kv_cache";
/// Load or compile progress as a fraction from 0.0 to 1.0 (float).
pub const MODEL_PROGRESS: &str = "model.progress";
