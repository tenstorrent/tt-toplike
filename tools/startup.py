#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.
"""Measure how tt-toplike starts: time to first output and frames per half second.

Usage:
    python3 tools/startup.py <binary> <args...> <seconds>

It runs the binary in a pseudo-terminal of 140x40 (override with the
STARTUP_COLS and STARTUP_ROWS environment variables), reads everything the
binary writes for <seconds>, then sends SIGTERM.

What it prints:
  * first output: when the first byte arrived (the text banner counts).
  * alt screen: when the binary entered the alternate screen (ESC [?1049h).
  * first frame: when the first ratatui frame finished. ratatui writes
    ESC [?25l (hide cursor) at the end of every `Terminal::draw`, so each one
    is counted as one frame. The splash screen does not use ratatui. Its
    first draw also hides the cursor, right after entering the alternate
    screen (ESC [?1049h ESC [?25l); a mark right after ESC [?1049h is not
    counted, so the splash's draws are not frames.
  * frames per half second, in buckets of 0.5 s counted from the first frame.
  * bytes per half second, counted from the start.

Originally written by the controller of the fast-start task as a byte counter;
the frame counter was added here so the acceptance figure (frames per half
second) is read from the binary's own output.
"""
import fcntl
import os
import pty
import select
import signal
import struct
import sys
import termios
import time

FRAME_MARK = b"\x1b[?25l"
ALT_MARK = b"\x1b[?1049h"


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(2)
    binp, args, secs = sys.argv[1], sys.argv[2:-1], float(sys.argv[-1])
    cols = int(os.environ.get("STARTUP_COLS", "140"))
    rows = int(os.environ.get("STARTUP_ROWS", "40"))
    pid, fd = pty.fork()
    if pid == 0:
        os.environ["TERM"] = "xterm-256color"
        os.environ["COLORTERM"] = "truecolor"
        os.execv(binp, [binp] + args)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    t0 = time.time()
    first = None
    alt = None
    total = 0
    marks = []  # (time, bytes)
    frames = []  # times of frame ends
    tail = b""  # last bytes of the previous read, so a mark split across reads is found
    while time.time() - t0 < secs:
        r, _, _ = select.select([fd], [], [], 0.05)
        if not r:
            continue
        try:
            d = os.read(fd, 65536)
        except OSError:
            break
        if not d:
            break
        now = time.time() - t0
        if first is None:
            first = now
        total += len(d)
        marks.append((now, len(d)))
        buf = tail + d
        if alt is None and ALT_MARK in buf:
            alt = now
        # Count marks that end inside the new data only.
        start = 0
        while True:
            i = buf.find(FRAME_MARK, start)
            if i < 0:
                break
            after_alt = buf[max(0, i - len(ALT_MARK)):i] == ALT_MARK
            if i + len(FRAME_MARK) > len(tail) and not after_alt:
                frames.append(now)
            start = i + 1
        tail = buf[-(len(ALT_MARK) + len(FRAME_MARK) - 1):]
    os.kill(pid, signal.SIGTERM)
    try:
        os.waitpid(pid, 0)
    except Exception:
        pass
    fmt = lambda v: "none" if v is None else f"{v:.2f}s"
    ff = frames[0] if frames else None
    print(f"first output {fmt(first)}; alt screen {fmt(alt)}; first frame {fmt(ff)}; "
          f"bytes total {total}; frames {len(frames)}")
    if ff is not None:
        b = {}
        for t in frames:
            k = int((t - ff) * 2) / 2
            b[k] = b.get(k, 0) + 1
        print("frames/0.5s from first frame: " +
              " ".join(f"{k / 2:.1f}:{b.get(k / 2, 0)}" for k in range(0, int((secs - ff) * 2))))
    b = {}
    for t, n in marks:
        k = int(t * 2) / 2
        b[k] = b.get(k, 0) + n
    print("bytes/0.5s: " + " ".join(f"{k:.1f}:{v}" for k, v in sorted(b.items())[:24]))


if __name__ == "__main__":
    main()
