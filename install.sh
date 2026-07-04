#!/bin/bash
# Install script for tt-toplike with multi-chip support and 256-color tmux compatibility

set -e

# Resolve to the directory containing this script so the script works
# regardless of where the user clones the repo.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ ! -f "$SCRIPT_DIR/Cargo.toml" ]; then
    echo "Error: could not find Cargo.toml next to install.sh" >&2
    exit 1
fi
cd "$SCRIPT_DIR"

echo "🎮 Installing tt-toplike (TUI + GUI)"
echo "========================================"
echo ""

# Build TUI
echo "Building TUI binary..."
cargo build --release --bin tt-toplike-tui --features tui

# Build GUI
echo "Building GUI binary..."
cargo build --release --bin tt-toplike-egui --features egui

# Install both
echo "Installing to ~/.local/bin/..."
cargo install --path . --bin tt-toplike-tui --features tui --force --root ~/.local
cargo install --path . --bin tt-toplike-egui --features egui --force --root ~/.local

echo ""
echo "✅ Installation complete!"
echo "   Installed to ~/.local/bin/: tt-toplike-tui, tt-toplike-egui"
echo ""
echo "Quick start:"
echo "  tt-toplike-tui --mock --mock-devices 4   # simulated telemetry, no hardware"
echo "  tt-toplike-tui --mode arcade             # launch straight into a visualization"
echo "  tt-toplike-tui --help                    # all options"
