//! Animation module for hardware-responsive visualizations
//!
//! This module provides the animation systems that make tt-toplike-rs special:
//! - Adaptive baseline learning that makes visualizations sensitive to any hardware
//! - Hardware-responsive starfield where every pixel reflects real telemetry
//! - Memory hierarchy "planets" showing L1/L2/DDR activity
//! - Data flow streams between devices based on actual power differentials
//!
//! ## Psychedelic Visualization Modes
//!
//! Inspired by Electric Sheep, LZX video synthesis, Logstalgia, TRON,
//! 1990s BBS ANSI art, and 1960s psychedelic visuals.
//!
//! The key philosophy: **Every visual element is informationally meaningful**.
//! No fake animations that move regardless of hardware state.

pub mod baseline;
pub mod starfield;
pub mod common;
pub mod tron_grid;

pub use baseline::AdaptiveBaseline;
pub use starfield::{HardwareStarfield, Star, MemoryPlanet, DataStream};
pub use tron_grid::TronGrid;
pub use common::*;
