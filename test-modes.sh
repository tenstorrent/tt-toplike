#!/bin/bash
# Test all visualization modes with --mode flag

echo "🎸 Testing Visualization Mode CLI Flags 🎸"
echo "==========================================="
echo ""

echo "Available modes:"
echo "  --mode normal     - Table view with telemetry"
echo "  --mode starfield  - Tensix cores as stars"
echo "  --mode castle     - Memory hierarchy (DDR → L2 → L1 → Tensix)"
echo "  --mode flow       - Full-screen DRAM motion"
echo "  --mode arcade     - Unified visualization with hero character"
echo ""

echo "Press Ctrl-C to exit each mode, or wait 3 seconds for automatic exit"
echo ""

# Test each mode
for mode in normal starfield castle flow arcade; do
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "Testing: --mode $mode"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

    # Launch with timeout (3 seconds) or Ctrl-C
    timeout 3 ./target/debug/tt-toplike-tui --mock --mock-devices 4 --mode $mode || true

    echo ""
    sleep 1
done

echo "✅ All modes tested!"
echo ""
echo "Try them yourself:"
echo "  cargo run --bin tt-toplike-tui --features tui -- --mode arcade"
echo "  cargo run --bin tt-toplike-tui --features tui -- --mode castle --backend sysfs"
echo "  cargo run --bin tt-toplike-tui --features tui -- --mode starfield --interval 50"
