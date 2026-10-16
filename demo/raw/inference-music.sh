#!/usr/bin/env bash
# Records the Inference view (`i`) beside a live audio server. The manifest
# cannot send keys to a pane, so this script runs the TUI in a private tmux
# session, presses `i` after the first paint, and attaches so asciinema sees
# the screen. The hybrid backend reads sysfs and tt-smi and never opens
# /dev/tenstorrent, so it cannot disturb the server being shown.
set -u
SOCK=ttdemo-inference
tmux -L "$SOCK" kill-server 2>/dev/null || true
tmux -L "$SOCK" new-session -d -s demo -x 160 -y 44 \
  "tt-toplike-tui --backend hybrid"
(
  sleep 5
  tmux -L "$SOCK" send-keys -t demo i
  sleep "${DEMO_SECS:-50}"
  tmux -L "$SOCK" send-keys -t demo q
) &
tmux -L "$SOCK" attach -t demo
wait
tmux -L "$SOCK" kill-server 2>/dev/null || true
