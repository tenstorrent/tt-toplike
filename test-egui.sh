#!/bin/bash
# Test script for egui dashboard

echo "═══════════════════════════════════════════════════════════"
echo "  🎸 TT-TOPLIKE-RS EGUI DASHBOARD - K-RAD EDITION 🎸"
echo "═══════════════════════════════════════════════════════════"
echo ""
echo "Launching with mock backend (4 devices)..."
echo ""

# Run egui dashboard with mock backend
./target/debug/tt-toplike-egui --mock --mock-devices 4

echo ""
echo "Dashboard closed. To run again:"
echo ""
echo "  # With mock backend:"
echo "  ./target/debug/tt-toplike-egui --mock --mock-devices 4"
echo ""
echo "  # With real hardware (sysfs):"
echo "  ./target/debug/tt-toplike-egui --backend sysfs"
echo ""
echo "  # With real hardware (json):"
echo "  ./target/debug/tt-toplike-egui --backend json"
echo ""
