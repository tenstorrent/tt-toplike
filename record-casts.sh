#!/usr/bin/env bash
# record-casts.sh — record asciinema casts of every tt-toplike visualization mode.
#
# Strategy: for each mode we spin up a fresh tmux session at a fixed terminal size,
# launch tt-toplike, inject keypresses via `tmux send-keys`, let it play, then quit.
# asciinema wraps the whole tmux pane so the recording captures real TUI rendering
# (including color, Unicode block chars, overlays, etc.).
#
# Usage:
#   ./record-casts.sh             # real hardware (4 BH chips expected)
#   ./record-casts.sh --mock      # mock backend

set -euo pipefail

BIN="tt-toplike-tui"
OUTDIR="assets/casts"
SESSION="ttcast"
COLS=200
ROWS=50

BACKEND_ARGS=""
if [[ "${1:-}" == "--mock" ]]; then
    BACKEND_ARGS="--mock --mock-devices 4"
fi

mkdir -p "$OUTDIR"

# Kill any leftover session from a previous run.
tmux kill-session -t "$SESSION" 2>/dev/null || true

# ─── helpers ──────────────────────────────────────────────────────────────────

# Send a literal string as key(s) to the tmux session, then sleep.
send() { tmux send-keys -t "${SESSION}" "$1"; }
pause() { sleep "${1:-1}"; }

# Record one mode.
# $1 = output filename (no extension)
# $2 = initial mode flag(s) passed to tt-toplike (e.g. "--mode defrag")
# $3 = shell script (here-string) of `send` + `pause` calls to drive the session.
record_mode() {
    local name="$1"
    local mode_flags="$2"
    local script="$3"
    local out="${OUTDIR}/${name}.cast"

    echo "▶  Recording: ${name} → ${out}"

    # New tmux session with fixed terminal size.
    tmux new-session -d -s "$SESSION" -x "$COLS" -y "$ROWS"

    # Launch tt-toplike inside the tmux pane (background so we can inject keys).
    tmux send-keys -t "$SESSION" "${BIN} ${BACKEND_ARGS} ${mode_flags}" Enter

    # Wait for TUI to paint its first frame.
    sleep 2

    # asciinema records the tmux pane output.
    # We pipe the injection script as a subshell so timing is relative to record start.
    asciinema rec "$out" \
        --cols "$COLS" --rows "$ROWS" \
        --title "tt-toplike: ${name}" \
        --command "bash -c '$(printf "%s" "$script")'" &
    local ASCI_PID=$!

    # Wait for the script (which runs inside asciinema's command slot) to finish.
    wait $ASCI_PID || true

    # Quit the TUI and clean up.
    tmux send-keys -t "$SESSION" "q" 2>/dev/null || true
    sleep 0.5
    tmux kill-session -t "$SESSION" 2>/dev/null || true

    echo "   ✓ saved ${out}"
    echo ""
}

# ─── Alternative: use a driver script that injects via tmux ───────────────────
# asciinema `--command` replaces the shell, so we need a different approach:
# record the tmux pane directly with `asciinema rec --command 'tmux attach -t SESSION'`
# and inject keys from the *outer* shell while recording runs.

record() {
    local name="$1"
    local mode_flags="$2"
    local duration="$3"   # total recording duration in seconds
    shift 3
    # remaining args: pairs of "SECONDS KEY" to inject
    local injections=("$@")

    local out="${OUTDIR}/${name}.cast"
    echo "▶  Recording: ${name}  (${duration}s)"

    tmux kill-session -t "$SESSION" 2>/dev/null || true
    tmux new-session -d -s "$SESSION" -x "$COLS" -y "$ROWS"
    tmux send-keys -t "$SESSION" "${BIN} ${BACKEND_ARGS} ${mode_flags}" Enter

    # Wait for first frame before attaching asciinema.
    sleep 2

    # Start recording — attach to the tmux session as the recorded process.
    asciinema rec "$out" \
        --cols "$COLS" --rows "$ROWS" \
        --title "tt-toplike: ${name}" \
        --overwrite \
        --command "tmux attach -t ${SESSION}" &
    local ASCI_PID=$!

    # Inject keypresses at scheduled offsets (measured from *now*, not TUI start).
    # The TUI has already been running for ~2s when recording begins.
    local t0
    t0=$(date +%s%3N)

    for (( i=0; i<${#injections[@]}; i+=2 )); do
        local delay="${injections[$i]}"
        local key="${injections[$((i+1))]}"
        local now
        now=$(date +%s%3N)
        local elapsed=$(( (now - t0) / 1000 ))
        local wait_s=$(( delay - elapsed ))
        if (( wait_s > 0 )); then
            sleep "$wait_s"
        fi
        tmux send-keys -t "$SESSION" "$key" 2>/dev/null || true
    done

    # Wait for total duration then quit.
    local now
    now=$(date +%s%3N)
    local elapsed=$(( (now - t0) / 1000 ))
    local remaining=$(( duration - elapsed ))
    if (( remaining > 0 )); then
        sleep "$remaining"
    fi

    # Detach the asciinema recording session (sends q to TUI then kills tmux).
    tmux send-keys -t "$SESSION" "q" 2>/dev/null || true
    sleep 1
    tmux kill-session -t "$SESSION" 2>/dev/null || true
    wait $ASCI_PID 2>/dev/null || true

    echo "   ✓ ${out}"
    echo ""
}

# ─── Recordings ───────────────────────────────────────────────────────────────
# Injection schedule: offset_seconds  key
# Keys:  l=legend  ?=help  !=explain  d=defrag  g=grid  q=quit
#
# Note: --mode CLI accepts: normal starfield castle flow arcade
# Defrag and Grid are only reachable via keypress (d / g) from inside the TUI.
# We start in a nearby mode then immediately press the target key.

# 1. Insights / Grid (default, press g immediately to confirm)
record "01-insights" "" 30 \
    2  "g" \
    4  "l" \
    7  "l" \
    9  "?" \
    12 "?" \
    14 "!" \
    17 "!" \
    19 "g"

# 2. Starfield
record "02-starfield" "--mode starfield" 28 \
    3  "l" \
    6  "l" \
    8  "?" \
    11 "?" \
    13 "!" \
    16 "!"

# 3. Memory Castle
record "03-memory-castle" "--mode castle" 28 \
    3  "l" \
    6  "l" \
    8  "?" \
    11 "?" \
    13 "!" \
    16 "!"

# 4. Memory Flow
record "04-memory-flow" "--mode flow" 28 \
    3  "l" \
    6  "l" \
    8  "?" \
    11 "?" \
    13 "!" \
    16 "!"

# 5. Defrag — start in normal mode, press 'd' to switch
record "05-defrag" "" 30 \
    2  "d" \
    4  "l" \
    7  "l" \
    9  "?" \
    12 "?" \
    14 "!" \
    17 "!"

# 6. Arcade (our showpiece — longer)
record "06-arcade" "--mode arcade" 35 \
    4  "l" \
    7  "l" \
    9  "?" \
    12 "?" \
    14 "!" \
    17 "!"

# 7. Host/CPU mode (no TT hardware needed — works anywhere)
record "07-host-cpu" "--host --mode starfield" 28 \
    3  "l" \
    6  "l" \
    8  "?" \
    11 "?" \
    13 "!" \
    16 "!"

echo "═══════════════════════════════════════════"
echo "✅ All recordings complete!"
ls -lh "${OUTDIR}"/*.cast
echo ""
echo "Upload casts to asciinema.org:"
echo "  asciinema upload ${OUTDIR}/<name>.cast"
echo ""
echo "Or play locally:"
echo "  asciinema play ${OUTDIR}/01-insights.cast"
