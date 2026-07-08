#!/bin/bash
# shoot-screenshots.sh — automated tt-toplike screenshots via tmux + spectacle
#
# Launches tt-toplike in a fullscreen konsole, cycles through every viz mode,
# and captures each one with spectacle.
#
# Usage: ./shoot-screenshots.sh [--mock] [--devices N]
#   --mock       use mock backend (no real hardware needed)
#   --devices N  number of mock devices (default: 4)

set -euo pipefail

OUTDIR="${HOME}/Pictures/tt-toplike-shots"
SESSION="ttshot"
MOCK_ARGS=""

# Parse args
DEVICES=4
for arg in "$@"; do
    case "$arg" in
        --mock)    MOCK_ARGS="--mock" ;;
        --devices) shift; DEVICES="$1" ;;
        --devices=*) DEVICES="${arg#--devices=}" ;;
    esac
done

if [ -n "$MOCK_ARGS" ]; then
    MOCK_ARGS="--mock --mock-devices ${DEVICES}"
fi

mkdir -p "$OUTDIR"
echo "📸 tt-toplike screenshot automation"
echo "   Output: $OUTDIR"
echo "   Args:   tt-toplike $MOCK_ARGS"
echo ""

# ── Helpers ────────────────────────────────────────────────────────────────
shot() {
    local name="$1"
    local file="$OUTDIR/${name}.png"
    # Small settle delay, then capture full screen
    sleep 0.4
    spectacle -b -n -f -o "$file"
    sleep 0.3
    echo "   📷 saved → ${name}.png"
}

send_key() {
    tmux send-keys -t "${SESSION}" "$1"
    # Give the TUI time to re-render the new mode
    sleep "$2"
}

# ── Clean up any leftover session ──────────────────────────────────────────
tmux kill-session -t "$SESSION" 2>/dev/null || true

# ── Start tt-toplike in a detached tmux session ────────────────────────────
# Large terminal size so multi-device layouts have room to breathe
tmux new-session -d -s "$SESSION" -x 220 -y 55 \
    "tt-toplike ${MOCK_ARGS}"

# ── Open a fullscreen konsole attached to that session ─────────────────────
konsole --fullscreen -e "tmux attach -t ${SESSION}" &
KONSOLE_PID=$!

echo "⏳ Waiting for konsole + TUI to settle..."
sleep 5

# ── Mode cycle order: Normal → Flow → Starfield → Castle → Arcade ──────────

echo "🖥  Shooting: info (normal) mode"
shot "01-info"

echo "🌊 Shooting: memory flow"
send_key "v" 3.5
shot "02-memory-flow"

echo "🌌 Shooting: starfield"
send_key "v" 3.5
shot "03-starfield"

echo "🏰 Shooting: memory castle"
send_key "v" 3.5
shot "04-memory-castle"

echo "🕹  Shooting: arcade mode"
send_key "v" 5
shot "05-arcade"

# ── Quit and clean up ──────────────────────────────────────────────────────
echo ""
echo "✅ Done! Closing konsole..."
send_key "q" 1
sleep 1
tmux kill-session -t "$SESSION" 2>/dev/null || true
kill "$KONSOLE_PID" 2>/dev/null || true

echo ""
echo "📦 Captured:"
ls -lh "$OUTDIR"/*.png
