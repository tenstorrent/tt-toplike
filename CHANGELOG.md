# Changelog

The **canonical, complete release log lives in [`debian/changelog`](debian/changelog)** —
that's the file the `.deb` packages are built from and where every release is
recorded in full. This file is a friendly pointer plus a summary of the most
recent releases; it deliberately does not duplicate the whole history.

To see everything:

```bash
less debian/changelog          # full history
git tag                        # released versions
```

## Recent releases

### 0.7.18
- **`--remote <host[:port]>`** — watch a remote QuietBox's telemetry over a
  WebSocket stream (plaintext, unauthed: trusted-LAN only). Every visualization
  runs against the remote chips; the process panel and `[i]` inference monitor
  still describe the **local** machine.
- Remote hardening: the process panel no longer TT-filters local processes under
  `--remote`, the Arcade `⚔` duel is suppressed under `--remote`, and backend
  status reports last-frame age / flags a stale stream.
- Packaging: WS support is a default-on `remote` cargo feature (opt out with
  `--no-default-features`).

### 0.7.17
- **Arcade duel** — the hero now duels the inference snake: a telemetry-true
  tug-of-war strip when a model is serving, the `⚔` marker sliding toward
  whichever side dominates (chip power/util vs tokens/s + queue depth).
- Per-device power/temp now shows once as a shared strip instead of per section.
- Memory Castle gains a compact 8-column tier before the fleet-grid fallback.
- 1990s BBS/demoscene ANSI chrome (`╔══[ SECTION ]══▓▒░`), themed under grayskull.

### 0.7.16
- **`/theme grayskull`** — an app-wide grayscale palette (a thousand shades of
  grey, cyan/purple accents, hot pink as the only hot color). `/theme default`
  restores full color; bare `/theme` toggles.
