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

pub mod arcade;
pub mod baseline;
pub mod common;
pub mod memory_castle;
pub mod memory_flow;
pub mod starfield;

pub use arcade::ArcadeVisualization;
pub use baseline::AdaptiveBaseline;
pub use common::*;
pub use memory_castle::MemoryCastle;
pub use memory_flow::MemoryFlowVis;
pub use starfield::{DataStream, HardwareStarfield, MemoryPlanet, Star};
