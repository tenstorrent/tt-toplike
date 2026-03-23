#!/bin/bash
# Install script for tt-toplike-rs with Arcade mode

set -e

echo "🎮 Installing tt-toplike-rs with Arcade Mode"
echo "=============================================="
echo ""

cd ~/code/tt-toplike-rs

# Build
echo "Building TUI binary..."
cargo build --release --bin tt-toplike-tui --features tui

# Install
echo "Installing to ~/.local/bin/..."
mkdir -p ~/.local/bin
cp target/release/tt-toplike-tui ~/.local/bin/tt-toplike-tui
chmod +x ~/.local/bin/tt-toplike-tui

echo ""
echo "✅ Installation complete!"
echo ""
echo "📍 Installed to: ~/.local/bin/tt-toplike-tui"
echo ""
echo "🎯 Usage:"
echo "  tt-toplike-tui --mock --mock-devices 4"
echo "  Then press 'A' to enter Arcade mode V2!"
echo ""
echo "✨ Features:"
echo "  • Starfield: Full width (40% height)"
echo "  • Bottom: Castle + Flow (left) | Device table (right)"
echo "  • Multi-chip: Memory Castle shows all 4 devices side-by-side!"
echo "  • btop++-inspired colors and crispness"
echo "  • Always see metrics + visualizations together!"
echo ""
echo "⌨️  Keyboard shortcuts:"
echo "  A/a - Jump to Arcade mode (V2 layout!)"
echo "  v   - Cycle visualization modes"
echo "  b   - Switch backend"
echo "  q   - Quit"
echo ""
