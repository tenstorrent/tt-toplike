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

### 0.11.0
- **Add**: Training view (`t`) — a full-screen visualization of a live
  tt-train run, drawn as the network it is: a character grid of transformer
  blocks and attention heads fed by token particles, a loss "mountain range"
  under a twinkling aurora nightscape that opens up as the model converges,
  plus chip telemetry alongside it.
- **Auto-attaches with no command**: scans running processes for a tt-train
  example binary (`nano_gpt`, `mnist_mlp`, `linear_regression`), then resolves
  `/proc/<pid>/fd/1` to find and tail that process's log. Checkpoint saves
  are detected by mtime on the run's rolling checkpoint file.
- **Honest limitation**: tt-train's per-step stream can only be tailed if its
  stdout was redirected to a real file at launch (`> train.log`). If fd 1 is
  a pipe or tty, retroactively reading it is an OS-level impossibility, not a
  gap in this tool — the view says so and falls back to what it can still
  see (process liveness, RSS/CPU, device fds, chip telemetry, checkpoint
  mtime) rather than drawing a fake loss curve. Gradient norms, MFU, and
  throughput counters are never available live either, so the view derives
  tokens/sec and ETA only from what it can actually read.
- Nine independent color channels (loss magnitude, run-history timeline,
  loss-delta direction, forward/backward sweep cadence, cache compile/steady
  state, checkpoint bursts, plus chip temp/power) — see the new legend and
  explain overlays.

### 0.10.3
- **Security fix**: a direct-vLLM host process's own `PATH` was forwarded
  onto the `sh` the monitor spawns to probe it — a bare `Command::new("sh")`
  lookup resolves via whatever `PATH` ends up on the builder, so an
  attacker-controlled process (gated only by `MESH_DEVICE`/`TT_METAL_HOME`,
  which the process itself controls) could have redirected the root-run
  monitor into executing a planted binary. Fixed by invoking the interpreter
  via an absolute path and never forwarding the target's own `PATH`.
- **Fix**: four GDDR-telemetry correctness gaps — a stale 8-channel cap
  dropping real fault data for Blackhole's channels 8–11, Starfield's DDR
  planets missing an `enabled` check present in every other memory
  visualization, the Insights GDDR temp/ECC rows silently disappearing
  instead of falling back on partially-usable real data, and a missing ECC
  counter parsing as a genuine zero instead of "not reported".
- **Perf**: a host-keyed service's per-tick probe redundantly re-read
  `/proc/<pid>/environ` up to four times and re-walked its process tree
  twice; now cached for the duration of one tick.

### 0.10.2
- **Fix**: the `[i]` Inference Server roster showed dozens of duplicate
  entries for a single real vLLM-on-TT launch — a real launch forks several
  worker/engine-core processes that each independently matched the
  direct-vLLM detection heuristic (each keyed by its own pid). Detection now
  walks each match's process ancestry and keeps only the root of each match
  family. Observed live: one real launch across 4 chips produced 26 roster
  entries, almost all stuck on "down".

### 0.10.1
- **Defrag view** now also drives per-channel training/enabled/temperature and
  errors from real `gddr_telemetry` when present, falling back to the
  existing packed pair-resolution registers otherwise. New states: a static
  "bad sector" row for a BIST-failed channel, and a brighter/longer flash for
  uncorrectable vs. correctable errors.
- **Fix**: a pre-existing EVICT false-positive that could trigger on idle
  hardware — the Idle/Init → Running transition captured its power baseline
  from a single raw sample instead of the smoothed EMA used elsewhere.
  Confirmed pre-existing (unrelated to the Defrag change above) via an A/B
  test against the prior commit.

### 0.10.0
- **Per-GDDR-channel telemetry** (tt-smi ≥ 6.3.0's `gddr_telemetry`: training/
  BIST pass, harvested/enabled, dual-location temperature, directional
  correctable/uncorrectable ECC) now flows into chip portrait, Memory Flow,
  Memory Castle, Starfield, and the Insights sidebar — replacing coarser
  packed-register decodes with real per-channel state wherever it's
  available, and falling back to today's exact behavior everywhere else
  (older tt-smi, sysfs, luwen, Wormhole/Grayskull). A whole-branch review
  caught what no single file's review could: the new Insights summary row
  was 47 columns in a 30-column budget and silently clipped its own
  headline BIST-fail count on real hardware — fixed alongside the sidebar's
  headroom invariant, a starfield DDR-planet rendering inconsistency on
  Blackhole (8 real channels vs. 12 physical), and the ECC row's per-channel
  sourcing. See `docs/superpowers/specs/2026-08-28-gddr-telemetry-design.md`.
  Not yet hardware-verified against a real tt-smi ≥ 6.3.0 box.

### 0.9.0
- **Direct (non-Docker) vLLM-on-TT detection for the `[i]` Inference Server
  panel.** Previously the panel only saw a `docker run`-launched
  tt-inference-server container; a bare `vllm serve <model> ...` /
  `server_example_tt.py --model <model> ...` process (as `tt-model serve`
  launches directly) was invisible to it. A new `Source::Host { pid }` variant
  and `SystemProbe` (wrapping the existing `DockerProbe`) let both deployment
  shapes be monitored simultaneously — the host path reads `/proc` and runs
  local `ps`/`sh` scoped to the launched pid's whole process tree rather than
  a Docker container. Requires `MESH_DEVICE`/`TT_METAL_HOME` present in the
  process's own environment to confirm it's TT-backed. Host-keyed liveness
  doesn't rely on matching a process name (a pip console-script's `comm`
  isn't `python3`) — any row in the tree-scoped `ps` output counts as alive.
  `host_exec` allowlists only the env vars the probe's own shell commands
  need, rather than copying a locally-launched process's full environment
  into a root-run monitor's spawned shell. See
  `docs/superpowers/specs/2026-08-27-direct-vllm-detection-design.md`.
  Not yet hardware-verified against a real `tt-model serve` launch.
- **`webbrowser` 1.2.0 → 1.2.4** (RUSTSEC-2026-0257). Lockfile only, and
  transitive through `egui-winit` → `eframe`, so it affects the optional egui
  GUI and not the shipped TUI binary.

### 0.8.0
- **Fix: per-device data was shuffled on multi-card boxes.** The sysfs backend
  numbered devices in raw `readdir` order while `tt-smi` orders by PCI bus id, so
  the default (hybrid) backend's join attributed every card's SMBUS data to the
  wrong card — DDR status, GDDR temps, ECC counters, thermal trips, fan and clocks
  all landed on a neighbour. Discovery is now sorted by bus id, **and the join
  itself keys on the PCI bus id** rather than on list position, so attribution
  also holds when `tt-smi` enumerates fewer cards than hwmon does (a busy card, a
  card that failed to enumerate, `--devices` filtering, hotplug). If you ran the
  default backend on more than one card, what you saw was mixed up.
- **The safe backend got a lot less telemetry-poor.** Alongside hwmon, the sysfs
  path now reads tt-kmd's class-attribute directory
  (`/sys/class/tenstorrent/tenstorrent!N/`): clock frequencies (AICLK/AXICLK/ARCCLK),
  ARC firmware heartbeat, the real board SKU, firmware bundle version, board serial
  and thermal-trip count. `tt_card_type` **replaces** the ~1.2 s `tt-smi -s` startup
  probe on modern drivers. Needs tt-kmd ≥ 2.7; older drivers keep the old behavior.
- **Live PCIe bandwidth.** tt-kmd's `pcie_perf_counters/` are folded into in/out
  directions and differentiated between ~1 Hz samples, so Insights shows a PCIe row
  with link geometry (e.g. `Gen4 x4`) and live ▼/▲ rates. A counter set that can't
  be read at all reports nothing rather than a confident 0 B/s. Sysfs and hybrid
  backends only.
- **Better hwmon reads**: sensors are picked by their `*_label` (so the ASIC temp
  sensor is used, not whichever has the lowest index), the fan sensor is read, and
  each sensor's own `*_max` gives real per-board limits (125 W / 500 A / 90 °C on a
  p300c) instead of hardcoded 300 W / 105 °C — a limit always comes from the same
  sensor as its reading. Limits need tt-kmd ≥ 2.9. The heavier reads (class attrs,
  PCIe counters) are sampled at ~1 Hz instead of on every render frame.
- **New Insights sidebar rows**: Current (with TDC limit), Board power (tt-smi 6.x),
  PCIe, GDDR ECC (only when non-zero — uncorrectable errors in red; they were parsed
  but never shown anywhere before), and thermal trips (only when non-zero).
- **tt-smi upkeep**: full `board_info` parsing (PCIe generation/width, tolerant of
  tt-smi's number-vs-string drift); the Fan row no longer stays blank on cards with a
  spinning fan (live tt-smi emits `FAN_SPEED: "0x0"` next to a real `FAN_RPM`, so
  whichever holds a non-sentinel reading now wins); tt-smi ≥ 6.0.0's top-level
  `processes[]` (per-device pid/user/cmdline attribution) and `telemetry.board_power`
  are parsed.
- **Fix: the real per-board limits didn't actually reach every backend.**
  `--backend json` silently dropped tt-smi's whole `limits` block (tt-smi emits
  those numbers as quoted strings, which the parser rejected) and the luwen
  backend never populated limits or the firmware version at all — both fell back
  to the generic 300 W / 105 °C. Fixed in both, and the limit shown next to Temp
  is now the ~90 °C *throttle* point on all four backends (it was tt-smi's 110 °C
  shutdown trip on the JSON path), so the same row means the same thing however
  you launch. luwen also gains the FW bundle version row; its limits are
  Blackhole-only, reported as absent rather than 0 on Wormhole.
- Both Prometheus scrapers accept either vLLM KV-cache metric name
  (`vllm:gpu_cache_usage_perc` or `vllm:kv_cache_usage_perc`) — vLLM renamed it and
  builds in the field ship one or the other.
- **luwen backend migrated** off the abandoned `all-smi-luwen-*` forks onto the
  official `luwen-api`/`luwen-pci`/`luwen-def` 0.8.5 crates, gaining Blackhole GDDR
  temps/ECC counters, harvesting and enabled masks, and thermal-trip count.
  **Verified on 4× Blackhole p300c**: all four chips detected, with power,
  current, ASIC and GDDR temperatures and ETH link state agreeing with the
  sysfs/hybrid path and `tt-smi -s`. Wormhole and Grayskull remain unverified
  (no such silicon to hand — their register layouts are covered by unit tests
  only), as does behaviour under load; every check so far ran against idle
  cards. It stays explicit-only (`--backend luwen`, never auto-detected, never
  in the `b` cycle): Luwen/UMD arbitration is unresolved upstream, and on
  tt-kmd ≥ 2.9/2.10 merely holding the device open participates in driver
  power-state aggregation and can block `O_EXCL` openers like `tt-flash`.
  Expect slower startup than sysfs (it scans PCI and reads a scratch register
  per chip) and no PCIe row (link counters and geometry have no luwen source).

### 0.7.33
- **[i] media/diffusion monitoring** — SkyReels / SDXL / z-image servers
  (tt-media-inference-server) now show live telemetry instead of a blank panel.
  They expose a `tt_media_server_*` Prometheus namespace (not `vllm:`), so a
  dedicated parser reads completed generations, the **in-flight `jobs_in_progress`
  gauge**, and per-generation timing. The Feeding snake is reused: headline is
  generations/min + seconds-per-gen, the body tracks in-flight jobs, and the
  panel shows in-flight/done + stage times (no tok/s). Verified against a live
  0.15.0 SkyReels box; dedupes the duplicate series that build emits.
- Fix: the legend / help / explain overlay panel truncated its own text (fixed
  42-col width vs 50–66-col content, and `Paragraph` clips instead of wrapping).
  It now measures its widest line and sizes to fit, clamped to the terminal.

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
