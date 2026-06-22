// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! tt-toplike — default cargo-install entry point
//!
//! `cargo install` produces a `tt-toplike` binary from this file.
//! It delegates immediately to the same main() as `tt-toplike-tui`
//! so users who run `cargo install tt-toplike` get a working tool
//! without needing to know the `-tui` suffix.
//!
//! The Debian package installs `tt-toplike-tui` as `/usr/bin/tt-toplike`
//! directly; this file covers only the `cargo install` code path.

fn main() {
    // Locate our own path and replace the process with tt-toplike-tui if it
    // exists next to this binary.  Fallback: exec from PATH.
    let self_path = std::env::current_exe().ok();
    let tui_name = "tt-toplike-tui";

    if let Some(dir) = self_path.as_ref().and_then(|p| p.parent()) {
        let sibling = dir.join(tui_name);
        if sibling.exists() {
            let err = std::os::unix::process::CommandExt::exec(
                std::process::Command::new(&sibling).args(std::env::args().skip(1)),
            );
            eprintln!("tt-toplike: failed to exec {:?}: {}", sibling, err);
            std::process::exit(1);
        }
    }

    // Try PATH
    let err = std::os::unix::process::CommandExt::exec(
        std::process::Command::new(tui_name).args(std::env::args().skip(1)),
    );
    eprintln!("tt-toplike: '{}' not found in PATH: {}", tui_name, err);
    eprintln!("Install with: cargo install tt-toplike --bin tt-toplike-tui");
    std::process::exit(1);
}
