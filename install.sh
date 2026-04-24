#!/bin/bash
# Install script for tt-toplike with multi-chip support and 256-color tmux compatibility

set -e

echo "🎮 Installing tt-toplike (TUI + GUI)"
echo "========================================"
echo ""

cd ~/code/tt-toplike-rs 2>/dev/null || cd ~/code/tt-toplike

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
echo ""
echo "📍 Installed binaries:"
echo "  • ~/.local/bin/tt-toplike-tui (2.3 MB)"
echo "  • ~/.local/bin/tt-toplike-egui (16 MB)"
echo ""
echo "🎯 Quick Start:"
echo "  # Mock backend"
echo "  tt-toplike-tui --mock --mock-devices 4"
echo ""
echo "  # Launch directly into modes:"
echo "  tt-toplike-tui --mode arcade"
echo "  tt-toplike-tui --mode castle"
echo "  tt-toplike-tui --mode starfield"
echo "  tt-toplike-tui --mode flow"
echo ""
echo "✨ New Features:"
echo "  • 256-color fallback for tmux (fixes macOS Terminal.app via SSH)"
echo "  • Multi-chip Memory Castle (side-by-side view for 2-4 devices)"
echo "  • --mode CLI flag for direct visualization launch"
echo "  • Improved contrast and btop++-inspired colors"
echo ""
echo "⌨️  Keyboard shortcuts:"
echo "  v   - Cycle visualization modes"
echo "  b   - Switch backend (Sysfs → JSON → Mock)"
echo "  q   - Quit"
echo ""
echo "🔧 Tmux Color Fix:"
echo "  Colors automatically use 256-color palette in tmux"
echo "  Fixes grey backgrounds in Terminal.app on macOS via SSH"
echo ""
