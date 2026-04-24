// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.


//! tt-toplike - Default Entry Point
//!
//! This is a convenience wrapper that launches the TUI by default.
//! For specific frontends, use the dedicated binaries:
//! - tt-toplike-tui: Terminal UI
//! - tt-toplike-gui: Native GUI

fn main() {
    eprintln!("Note: Building with default binary is deprecated.");
    eprintln!("Please use one of the specific binaries:");
    eprintln!();
    eprintln!("  Terminal UI:");
    eprintln!("    cargo run --bin tt-toplike-tui");
    eprintln!();
    eprintln!("  Native GUI:");
    eprintln!("    cargo run --bin tt-toplike-gui --features gui");
    eprintln!();
    eprintln!("For installation:");
    eprintln!("    cargo install --path . --bins");
    std::process::exit(1);
}
