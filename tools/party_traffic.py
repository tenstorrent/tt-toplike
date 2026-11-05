#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.
"""Measure the terminal traffic of a TUI view: bytes written per second.

Usage:
    python3 tools/party_traffic.py <binary> [--runs N] [--secs S] [--from A] [--to B] -- <args...>

Example (the Saga party view's budget check, 20 KB/s at 140x40):
    python3 tools/party_traffic.py target/release/tt-toplike-tui --runs 3 -- --mock 32 --mode saga

It uses the method of tools/startup.py: the binary runs in a pseudo-terminal
of 140x40 (override with the STARTUP_COLS and STARTUP_ROWS environment
variables) with TERM=xterm-256color and COLORTERM=truecolor, and every byte
it writes is counted. Each run lasts --secs seconds (default 40) and is then
sent SIGTERM. The rate is the bytes written from --from to --to seconds
(default 20 to 40) divided by that span, so start-up and the first world's
birth are left out.

Runs are made one after the other, never at once: two runs sharing the
machine slow each other and change the numbers. Each run's rate is printed,
then the mean (1 KB is 1,000 bytes). The rate moves by a few KB/s from run
to run, because the mock backend's story differs from session to session.

Set XDG_CONFIG_HOME to an empty directory to keep a personal config.toml out
of the measurement.

To see which part of the party view sends the bytes, the lib test
`print_party_traffic` (src/ui/tui/saga_party_tests.rs) renders consecutive
frames and splits each frame's changes by source.
"""
import argparse
import fcntl
import os
import pty
import select
import signal
import struct
import sys
import termios
import time


def checked_binary(path):
    """Resolve `path` to an absolute path and require an executable regular file.

    The path comes from the command line and is handed to exec, so it is
    checked before the fork. No shell is involved: the binary and its
    arguments go to execv as a list, and nothing is interpreted as code.
    """
    full = os.path.realpath(path)
    if not (os.path.isfile(full) and os.access(full, os.X_OK)):
        sys.exit(f"not an executable file: {path}")
    return full


def one_run(binp, args, secs, a, b, cols, rows):
    """Bytes written between `a` and `b` seconds of one run of `secs`."""
    pid, fd = pty.fork()
    if pid == 0:
        os.environ["TERM"] = "xterm-256color"
        os.environ["COLORTERM"] = "truecolor"
        try:
            os.execv(binp, [binp] + args)
        finally:
            # Only reached if exec failed. Without this the child would
            # carry on running this script's own code as a second process.
            os._exit(127)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    t0 = time.time()
    counted = 0
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
        if a <= now < b:
            counted += len(d)
    os.kill(pid, signal.SIGTERM)
    try:
        os.waitpid(pid, 0)
    except Exception:
        pass
    return counted


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("binary")
    p.add_argument("--runs", type=int, default=1)
    p.add_argument("--secs", type=float, default=40.0)
    p.add_argument("--from", dest="a", type=float, default=20.0)
    p.add_argument("--to", dest="b", type=float, default=40.0)
    # The binary's own arguments follow `--`, so they are never read as ours.
    argv = sys.argv[1:]
    cut = argv.index("--") if "--" in argv else len(argv)
    o = p.parse_args(argv[:cut])
    args = argv[cut + 1:]
    if not (0 <= o.a < o.b <= o.secs):
        sys.exit("need 0 <= --from < --to <= --secs")
    binp = checked_binary(o.binary)
    try:
        cols = int(os.environ.get("STARTUP_COLS", "140"))
        rows = int(os.environ.get("STARTUP_ROWS", "40"))
    except ValueError:
        sys.exit("STARTUP_COLS and STARTUP_ROWS must be whole numbers")
    if not (1 <= cols <= 1000 and 1 <= rows <= 1000):
        sys.exit("STARTUP_COLS and STARTUP_ROWS must be between 1 and 1000")
    span = o.b - o.a
    rates = []
    for i in range(o.runs):
        n = one_run(binp, args, o.secs, o.a, o.b, cols, rows)
        rate = n / span / 1000
        rates.append(rate)
        print(f"run {i + 1}: {n} bytes from {o.a:.0f} s to {o.b:.0f} s = {rate:.1f} KB/s")
    print(f"{cols}x{rows} {' '.join(args)}: mean {sum(rates) / len(rates):.1f} KB/s "
          f"over {len(rates)} run(s)")


if __name__ == "__main__":
    main()
