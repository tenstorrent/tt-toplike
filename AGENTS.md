# TT-Toplike-RS Development Log

Rust implementation of tt-top (Python) for real-time Tenstorrent hardware
monitoring. Binaries: `tt-toplike` (dispatcher), `tt-toplike-tui`,
`tt-toplike-app`/`tt-toplike-egui` (egui GUI).

> **Agent guidance lives in this file.** This project uses `AGENTS.md` (no
> separate `CLAUDE.md` — the two used to be a symlink; the symlink is gone as
> of Sept 2026, `AGENTS.md` is the sole source) for project notes,
> conventions, and the development log. Log "what happened?" succinctly: the
> prompt, key decisions, and notable moments — this file was condensed
> September 2026 after growing to 5,300+ lines; keep new entries tight.

This log is condensed: each phase below is compressed to origin, root
cause/decision, and the lesson worth keeping. Full code diffs, build
transcripts, and screenshots live in git history — `git log --grep "Phase
N"` or `git show <version tag>` if you need the original detail.

---

## Build & test gotchas (current)

* A `build-deb.sh` run leaves an **untracked** `.cargo/config.toml` + partial
  `vendor/` in the tree that redirects all crate lookups to `vendor/` for
  network-free dpkg builds. That vendor set is incomplete (missing
  `all-smi-luwen-*` and most deps), so a normal `cargo build` fails with
  *"no matching package named `all-smi-luwen-core`"*. Move `.cargo/config.toml`
  aside to build/test against the real registry cache, then restore it.
* After `cargo build --release`, copy every shipped binary into `~/.local/bin`
  (`cp target/release/tt-toplike{,-tui} ~/.local/bin/`) or a stale one already
  on `$PATH` shadows it. `install.sh` handles `tt-toplike-tui`/`tt-toplike-egui`
  via `cargo install` but not the `tt-toplike` dispatcher — copy that by hand.
* `all-smi-luwen-*` crates are **optional**, gated behind the `luwen-backend`
  feature — default builds don't need them.
* Adding a field to `ServiceState` / `TickSample` / `RemoteInference` /
  `SmbusTelemetry` is a compile error at **every** struct literal, including
  test-only ones — update all enumerated sites (the test build is where a
  missing field bites). `smbus_smooth::apply_ema` enumerates `SmbusTelemetry`
  fields by hand and has silently dropped new non-string fields twice
  (Phase 25, Phase 28) — check it explicitly, the compiler won't.
* TUI convention: **left-side and bottom borders only, never right-side border
  characters** (`╔`/`║`/`╚`) — variable-width box glyphs wrap when the terminal
  is even one column narrower than expected. Panels/overlays must size to their
  widest content in real display columns (unicode-width), never clip.
* A `Paragraph` clips instead of wrapping — any fixed-width panel/sidebar row
  needs its width budget checked against a *worst-case* test fixture (one
  that actually sets the fields being measured), or a new row can silently
  overflow and clip its own headline number (bit twice: legend overlay
  Phase "July 2026", Insights GDDR row Phase 28).
* CI runs every job with `cargo … --locked`; a version bump must regenerate
  `Cargo.lock` **before** committing or all `--locked` jobs fail.
* `cargo clippy --lib --features tui -- -D warnings` (the literal CI
  invocation) differs from `cargo test --lib`: code used only under
  `#[cfg(test)] mod tests` compiles under `test` but can dangle as "unused"
  under clippy's exact flags. Run the real CI command, not just the test
  suite, before signing off (bit Phase 27).
* A relative path is meaningless without the cwd it was written against, and
  a file watcher on a nonexistent path fails **silently and permanently**
  rather than erroring — always resolve a target process's own relative
  paths via its `/proc/<pid>/cwd`, never the monitor's own cwd (bit the
  Training view's checkpoint watcher twice — once for the config path, once
  for the checkpoint path itself, Phase 32/33).
* Position-vs-identity: never index into a `Vec` of per-channel/per-device
  data by loop position — index/look up by the real id field (`.channel ==
  idx`, `device.index`). A dropped malformed entry, a sparse tt-smi snapshot,
  or an ARC-dead chip all silently misattribute state otherwise (recurred at
  least 3 times: Phase 25 device ordering, Phase 28 GDDR channels, Phase 36
  Compact-tier header).
* An "instrument" (a status row, a phase detector) that reports a confident
  reading from data it never actually received is worse than reporting
  nothing — distinguish "measured zero" from "no data" explicitly (ETH
  `0/12 live` bug, Defrag phantom `4×RUN` on idle hardware; Phase 34).
* Forwarding a target/child process's own environment (or its self-reported
  `PATH`) into a locally-spawned shell is a privilege-escalation vector on a
  shared box — allowlist specific env vars instead of copying wholesale, and
  invoke interpreters by absolute path (`/bin/sh`), never a bare-name lookup
  that resolves against an attacker-influenced `PATH` (Phase 27, Phase 31).

---

## Phase 0–1: Planning & Foundation (Jan 11, 2026)

Prompt: "I want to see what this app would be like written in Rust." Research
surfaced **luwen** (official TT Rust hardware-access lib) and **all-smi**
(third-party monitor built on it) — decided on a hybrid backend: Luwen
(direct hardware) primary, JSON (tt-smi subprocess) fallback, user-selectable.
Foundation: `thiserror`-based error types, `Architecture` enum (Grayskull/
Wormhole/Blackhole with per-arch DDR-channel counts and Tensix grid
dimensions), all telemetry fields `Option<T>` for graceful degradation.
Gotcha: `chrono`'s `serde` feature must be enabled explicitly for `DateTime`
serialization.

## Phase 2–4: JSONBackend, CLI, TUI (Jan 11, 2026)

JSONBackend spawns tt-smi as a subprocess with a threaded reader; parser
tries wrapper-format before single-device format (order matters — a wrapper
object can false-match as a single device). CLI via clap: `--backend
[auto|mock|json|luwen]`, `--mock`/`--json` shortcuts, `--devices` filter,
auto-detect tries JSON then falls back to Mock. TUI via Ratatui/Crossterm:
color-coded telemetry table, `q`/`r`/Esc controls, alternate-screen terminal
management.

## Phase 5: Hardware-responsive visualizations (Jan 11, 2026)

Introduced `AdaptiveBaseline`: learns each device's idle power/current/temp
over the first ~20 samples, then shows all activity as *relative change from
baseline* rather than an absolute threshold. This is the load-bearing design
decision behind every later visualization — it makes them universally
sensitive regardless of a given box's absolute power range (a 10% bump reads
the same on a 20W idle chip as a 200W one), and its absence is exactly what
caused the Phase 34 Defrag bug decades later. Starfield: star position = real
Tensix grid topology, color = temperature, brightness = power, twinkle rate =
current; memory "planets" for L1/L2/DDR.

## Phase 6: Luwen backend (Jan 11, 2026)

`detect_chips_silent()` for device discovery, full SMBUS telemetry mapping.
Added `IsTerminal` TTY check before TUI init (clear error instead of a
confusing crash over SSH/pipes).

## Phase 7: Dark mode (Jan 12, 2026)

User: "stay dark mode we're in the terminal too." Removed all background
colors (use terminal default), brightened every foreground color 30-50%,
rounded borders throughout. Lesson: web-app color palettes do not transfer to
terminal UIs — terminal users expect dark backgrounds and bright,
high-contrast foregrounds.

## Phase 8–9: Psychedelic TRON Grid → real DDR/memory hierarchy (Jan 2026)

First TRON Grid cut used static color schemes ("The TRON mode just looks
red. What's the point?") — fixed with full HSV rainbow cycling (position +
time + temperature driven hue) and multi-layer sine-wave interference.
Second round of feedback ("still offers little value") pushed it from
decorative to informational: parsed the real SMBUS `DDR_STATUS` bitmask
(4 bits/channel: trained/training/untrained/error, animated while training)
plus L2 wave activity and a compressed L1 Tensix grid, replacing abstract
colored nodes with actual hardware state. Border-alignment lesson from this
era, later reaffirmed in Phase 16: precise unicode-width math is fragile
(emoji like ⛩ are 2 columns) — a fixed-length separator with consistent
padding often looks just as good and breaks less.

## Phase 10–12: GUI Dashboard, Luwen panics, Sysfs backend (Jan 15, 2026)

Dashboard became the GUI's default view (DDR channels + memory hierarchy +
animated gauges). Testing surfaced that `all-smi-ttkmd-if` **panics** (not
`Result::Err`) when BAR0 PCI mapping fails without permissions or while
active workloads hold the hardware — this crashed the whole app before
JSON/Mock fallback could run. Fixed with `std::panic::catch_unwind` around
Luwen init so the fallback chain actually works. Even with `noc_safe` mode,
Luwen still can't get a BAR0 mapping while an LLM/training job holds the
hardware — so a **SysfsBackend** was added, reading `/sys/class/hwmon/`
directly (kernel-mediated, zero PCI access, safe under concurrent readers,
works while hardware is busy). Trade-off: no SMBUS/firmware/DDR-training
detail, only what hwmon exposes.

## Phase 13: JSON backend redesign + live backend switching (Jan 15, 2026)

Original JSONBackend assumed tt-smi streams JSON line-by-line; in reality
`tt-smi -s` emits one complete snapshot and exits. Redesigned from
persistent-subprocess-with-threading to run-once-per-tick, matching the real
nested schema (`device_info[]` → `board_info`/`telemetry`/`smbus_telem`).
Also added a backend factory + `b` key to cycle backends live (Sysfs → JSON
→ Luwen → Mock) without restarting, requiring the TUI to own
`Box<dyn TelemetryBackend>` instead of borrowing it.

## Phase 14: Safe mode by default (Feb 26, 2026)

User: "Luwen should only be by explicit command." Auto-detect order changed
from Luwen→JSON→Sysfs→Mock to Sysfs→JSON→Mock; Luwen removed from default
Cargo features entirely and made reachable only via explicit `--backend
luwen`. This safety posture has held ever since (reaffirmed permanently in
Phase 25 for a driver-level reason, see below).

## Phase 15–16: Border/warning/scaling cleanup, Memory Dungeon overhaul (Mar 2026)

Fixed hardcoded pixel-width border math that broke on 2-column-wide emoji
(use `chars().count()`, not `len()`); zeroed out compiler warnings; GUI
switched from fixed pixel layouts to percentage-based so it scales with
window size. Memory Dungeon ("still not great" feedback) went from 200 to
600 particles across 4 distinct types (Read/Write/CacheHit/CacheMiss) with
trails, unified single-canvas rendering, and higher saturation — and, per
user insight, ditched precise per-line width calculations for the legend in
favor of a simple fixed-length separator ("choose an approach that gets a
similar result without the calculations... something that looks good
whether it's right, wrong, inside out or void").

## Phase 17–19: Arcade Mode, perf, multi-chip Memory Castle (Mar 19–20, 2026)

Arcade Mode composites Starfield + Memory Castle + Memory Flow into one
screen with a roguelike hero (`@`) whose position/color/pulse are driven by
power (Y), current (X), temperature (color), and heartbeat (pulse) — a
five-telemetry-dimension aggregator including its fading trail. Arcade was
then found sluggish vs. three separate terminals (it renders ~5,000
particle/glyph calcs per frame); fixed with density-reduced constructors
(50% fewer particles) used only in Arcade, while standalone modes kept full
density — and the vibrant color treatment Arcade had was extended to every
standalone mode too, since they'd been left on the old muted palette.
Separately, Memory Castle was found to only ever visualize `device[0]` on a
4-chip box — added per-particle `source_device` tracking, side-by-side
per-device columns, and an HSV hue shift per device for visual separation.

## Phase 20–21: Debian packaging, hardware plurality (Apr 2026)

Two .deb packages (`tt-toplike` TUI, `tt-toplike-egui` GUI) with safe default
features, offline vendored builds (`cargo vendor` + `--frozen`, required for
network-free Debian build daemons), and `Recommends: tt-smi,
tenstorrent-dkms`. Then: topology code hardcoded `chips_per_board = 2`, so
single-chip PCIe cards (p150a etc.) were mislabeled "Board 0/Board 1" instead
of standalone cards — fixed with `is_single_chip_card()`/
`has_multi_chip_boards()`, plus a fleet-grid mode that auto-scales for 32+
chips. Also shipped a noble/jammy .deb build matrix after discovering a
24.04-built binary requires `libc6 >= 2.39`, which 22.04 doesn't have.

## July 2026 — legend truncation, media-server metrics (v0.7.31–33)

Overlay panels hardcoded a 42-column width while content ran 50-66 columns —
`Paragraph` clips rather than wraps, so long lines silently lost their tail.
Fixed by measuring the widest real content line via `unicode-width` and
sizing the panel to fit. Separately, diffusion/video servers
(tt-media-inference-server: SkyReels, SDXL, z-image) showed a blank
inference panel because the metrics parser only recognized the `vllm:`
namespace; they expose `tt_media_server_*` on the same `/metrics` endpoint.
First cut used doc-derived metric names that don't exist on the real server
(`tt-media-inference-server:0.15.0`) — corrected against a live `curl
:8000/metrics`: real signals are `tt_media_server_requests_base_total`,
`tt_media_server_jobs_in_progress` (the missing "in-flight" signal),
`_duration_seconds_total`. Also found: that server's Prometheus
multiprocess mode emits every series **twice, byte-identical** — the
summing parser doubled everything until deduped by `name{labels}` identity.
Lesson: don't trust doc-derived metric names — curl the real endpoint.
⚠️ Metric shape is version-specific; re-curl when the server image changes.

## Phase 22 (design only, not built as its own phase) — Inference Server Monitor

Origin: watching Z-Image-Turbo sit silent for 90+ minutes during first-run
kernel compilation with no progress signal. Design established the
discriminating principle later implemented: CPU% alone can't tell "stuck"
from "compiling" (a tight loop and a real compile both peg CPU); the real
signals are independent progress probes — `.o` file count growth (compiling),
RSS growth (weight loading), open `.safetensors` fds (load cross-check). All
three flat simultaneously = the alarm condition. This shaped the Phase 26+
inference-monitoring work below.

## Phase 23–24: HivemindSweeper (Jul 14–16, 2026, v0.7.34–0.7.40)

New opt-in, read-only `~` mode correlating driver messages (`/dev/kmsg`),
tt-metal compile-cache churn, host/docker log tails, `/proc` device-fd
activity, and the tt-metal Inspector log into one `source × device/host`
decaying heat board plus a filterable live event feed. Built from a 12-task
spec-driven plan: bounded ring buffer of classified events, a
panic-isolated `Collector` trait (each source runs on its own thread behind
`catch_unwind` so one bad collector can't take the engine down), five
auto-spawned collectors, and a user-triggerable `wrap` collector (tail a
file / watch a pid's device-fds / spawn a command — no shell, no env
injection). `/watch` and `/wrap` commands, `hjkl` cursor nav, severity
filter. Hardening pass afterward fixed: a key-binding collision (`l` bound
to cursor-right, shadowing the global legend key); `procfs` classifying
almost everything as "unknown" (fixed by classifying on cmdline/loaded
libraries, not just process `comm`, plus a pid→source cache so closes match
their opens); a fan-RPM display bug (firmware's all-ones sentinel
`0xFFFF`/`0xFFFFFFFF` means "not reported," not literally 65535 RPM); added
a KITT-style activity scanner bar and `--mode hivemind` CLI alias.

## Phase 25: Telemetry expansion (Aug 2026, v0.8.0)

Ten-task effort closing gaps in both safe (sysfs) and Luwen telemetry paths:

- **Sysfs**: switched from fixed sensor indices to label-matched hwmon
  attributes (`*_label`); real per-board limits from `*_max` files instead of
  hardcoded constants. Discovered a *second* tt-kmd sysfs surface,
  `/sys/class/tenstorrent/tenstorrent!N/`, exposing static attrs
  (`tt_card_type` — replaces a ~1.2s `tt-smi` startup probe entirely) and
  dynamic ones (`tt_aiclk`, `tt_heartbeat`) that sysfs mode previously
  hardcoded to `None`.
- **Live PCIe bandwidth**: reads tt-kmd's `pcie_perf_counters/`, exposed as a
  new backend trait method that defaults to `None` so only sysfs/hybrid
  need implement it.
- **tt-smi schema drift**: tolerant parsing for `board_info` (PCIe
  speed/width sometimes numeric, sometimes string), a real fan-telemetry bug
  (`FAN_SPEED` and `FAN_RPM` are *both* present on Blackhole but only one is
  real — the other is a `0x0` sentinel, so `.or()` silently picks the wrong
  one; needed an explicit sentinel-aware preference), tt-smi 6.x's new
  `processes[]` array, and inconsistent vLLM KV-cache metric names across
  builds (`gpu_cache_usage_perc` vs `kv_cache_usage_perc`).
- **Luwen crate migration**: moved off a 13-months-stale third-party fork
  (`all-smi-luwen-*`) onto the official `luwen-api`/`luwen-pci`/`luwen-def`
  crates — gained Blackhole GDDR temps/ECC, harvesting masks, thermal trips.
  `luwen-api`'s own `detect_chips_silent` has **no hardware transport** (that
  lives in `luwen-pci`) and a different argument contract than the old
  fork's — the first migration attempt called it with an empty root-chip
  list and silently detected nothing.
  Hardware-verified live on 4× idle Blackhole; **not** verified on Wormhole/
  Grayskull or under load. Verification itself found real bugs: Blackhole
  never assigns per-ARC health registers (only a shared heartbeat), so a
  hardcoded `Some(0)` there made the stall detector flag every healthy card
  as `Stalled`; `thm_limits` was a raw, undecoded register used directly as
  a °C trip point; ETH numerator (`eth_live_status`) was left unpopulated
  while the denominator was, producing `0/12 live` on healthy links.
- **Device-index ordering bug** (safe path): `SysfsBackend` assigned device
  indices in raw `readdir` order — not stable across boots — while
  `HybridBackend` joins tt-smi metadata to it *by index*, so every card
  could silently display another card's telemetry. Fixed by sorting by bus
  ID before assigning indices.
- **Why Luwen stays launch-only forever**: beyond the original "can disrupt
  workloads" reasoning (Phase 14), holding `/dev/tenstorrent/N` open on
  modern tt-kmd participates in driver-managed power-state aggregation
  across *all* openers and can block `O_EXCL` openers like `tt-flash` — an
  idle monitor has no business being a silent party to another tool's
  exclusive-access contract.

## Phase 27: Direct (non-Docker) vLLM detection (Aug 27, 2026, v0.9.0)

The `[i]` Inference Server panel only detected Docker-launched servers.
Added `Source::Host { pid }` alongside `Source::Docker { container }`, and
`parse_direct_vllm` to recognize a bare `vllm serve <model>` or
`server_example_tt.py` launch — gated on `MESH_DEVICE`/`TT_METAL_HOME` being
present in *that process's own* environment (no `/dev/tenstorrent` cmdline
analogue for a bare host process). `SystemProbe` scopes CPU/RSS to the whole
process tree the launch spawned (compile children, device workers), since a
bare process has no cgroup boundary the way a container does. Review caught
two defects invisible to any single task: a liveness heuristic calibrated
for `python3` entrypoints misread a pip console-script's process name and
would have reported `Down` for the entire silent compile/load window this
feature exists to illuminate; and `host_exec` forwarded the target's full,
self-reported environment (a `LD_PRELOAD`-class risk) instead of an
allowlist. **Not hardware-verified** — no real `tt-model serve` launch was
available while building this.

## Phase 28: Per-GDDR-channel telemetry (Aug 28, 2026, v0.10.0)

tt-smi ≥ 6.3.0 exposes real per-channel GDDR data (training/BIST/harvested/
enabled/dual-location temp/directional ECC), replacing the old packed
4-bit-per-channel register. Wired into five visualization consumers (chip
portrait, Memory Flow, Memory Castle, Starfield, Insights sidebar), all
falling back to the old packed-register decode when the new block is
absent. The whole-branch review (after each of 9 tasks already passed its
own scoped review) still found: the new Insights row overflowed the
sidebar's column budget because the test fixture measuring it never set the
new field; Starfield rendered 8 real channels next to 4 fallback ones
inconsistently on Blackhole because the loop bound came from
`memory_channels()` (12) instead of the real channel list length; and a
spec requirement (ECC row should source from real per-channel data) was
simply dropped during plan-writing and the task reviewer verified the
*omission* as correct. **Not hardware-verified** — no tt-smi ≥ 6.3.0 box was
available at ship time.

## Phase 29: Defrag real data + idle-EVICT bug (Aug 28, 2026, v0.10.1)

Once a real tt-smi 6.3.0 box was available, wired Defrag's channel state to
it (new `bist_fail`/`uncorr_flash` visual states). User then reported
eviction happening "even on idle" — rather than assume the new change was
the cause, built the previous commit's binary in an isolated worktree and
A/B-tested both live: **both** evicted repeatedly, proving the bug
pre-existed. Root cause: `idle_power` (the eviction baseline) was captured
from a single unsmoothed raw power sample at the Idle→Running transition
instead of the smoothed `power_ema` used at the other two capture sites —
one noisy low sample could set an artificially low baseline and trigger a
false evict cycle on a chip that never actually loaded anything.

## Phase 30: Inference roster dedup (Aug 28, 2026, v0.10.2)

A real multi-chip `tt-model serve` launch showed "26 devices, +18 more" in
the roster. Root cause: `fork()` preserves the parent's full cmdline and
environment, so every vLLM worker/engine-core child process independently
matched the Phase 27 detection heuristic and was counted as its own
service (keyed per-pid). Fixed with `is_match_root`, which walks a matching
process's ancestry and drops any match whose ancestor also matches — keeps
only the root of each family. Found from a live screenshot; the fix itself
was not re-observed live before release.

## Phase 31: `/code-review` findings (Aug 28, 2026, v0.10.3)

Six findings from a full-branch review, all resolved:
1. **PATH injection (Critical)** — `host_exec` forwarded the target
   process's self-reported `PATH` onto a bare `Command::new("sh")` lookup;
   empirically verified exploitable with a throwaway test harness before
   fixing. Fixed with an absolute `/bin/sh` path and a fixed conservative
   `PATH`. First regression-test draft was vacuous (faked the wrong binary,
   passed against both vulnerable and fixed code) — rewritten to fake `sh`
   itself and confirmed genuinely red/green.
2. **Four GDDR correctness gaps** — a leftover 8-channel index cap
   discarding Blackhole's channels 8-11; Starfield checking `harvested` but
   not `enabled`; two rows falling back to old data only on `None`/empty,
   not on "present but yields nothing usable"; ECC counters as plain `u64`
   conflating "missing" with "zero" (widened to `Option<u64>` across ~5
   files — a bigger refactor, explicitly confirmed with the user first).
3. **Redundant per-tick I/O (Minor)** — one probe cycle re-read
   `/proc/<pid>/environ` up to 4× and re-walked the process tree twice,
   spawning two `ps` subprocesses; fixed with a 500ms per-pid memoization
   cache (asked the user before doing this one too, given it touched the
   same code as the security fix).

## Phase 32–33: Training view (Aug 29–30, 2026, v0.11.0–0.11.1)

New view watching live tt-train (a compiled C++ binary with no dashboard of
its own — every line shape verified against the real source, not docs).
Auto-attaches via `readlink /proc/<pid>/fd/1`: a regular file (stdout
redirected to a log) is tailed live; a pipe or tty is honestly reported as
un-recoverable rather than faking a curve. Nine independent color channels
encode loss magnitude, forward/backward pass sweep direction, run history,
loss delta direction, chip temp/power, kernel-cache compile state, and
checkpoint saves (detected via mtime polling, the same technique
HivemindSweeper uses for compile-cache churn). Four plan defects were
caught during task review, not by faithful transcription: a false
auto-attach claim in the explain overlay, legend colors that didn't match
the real renderer, a `v`-cycle refactor brief that silently dropped
pre-existing side effects, and a `CheckpointWatch` that swallowed the very
first save of a fresh run (fixed with an `existed_at_start` flag, TDD'd to
fail before the fix landed). Recording a demo of the view (`tt-demo verify`
contact-sheet check) found two more real bugs: the animation was running at
the 10fps *data* rate instead of the 60fps *animation* rate (missing from
`is_anim_mode`); and the checkpoint comet never fired at all, because
`model_path` was resolved against the monitor's own cwd instead of the
trainer's (via `resolve_for_pid`, which already existed for the config path
and simply wasn't called for this one). **Not hardware-verified** against a
real tt-train run when shipped (later verified live in Phase 34's window —
see v0.13.2 in git log).

## Phase 34: Review findings — launch coverage + two silent-instrument bugs (Sep 1, 2026)

Independent hardware review of PR #26 on 4× p300c found three non-blocking
issues:
1. `parse_direct_vllm` only recognized 1 of 4 real vLLM launch shapes in use
   across TT tooling (missed the bare `python -m vllm.entrypoints...` module
   form and tt-inference-server's `run_vllm_api_server.py` wrapper, which
   also spells its port flag `--service-port`). Widening the match made a
   dedup contract load-bearing: a container match found via image/cmdline
   inspection could now double-count against the same server found via the
   host-process scan, so host-path detection now explicitly drops any pid
   inside a Docker cgroup segment.
2. ETH row printed a confident `0/12 live` when liveness data was simply
   absent rather than measured zero — reproduced with a real doctored
   tt-smi snapshot; fixed to show `?/12` (no dots) when there's nothing to
   draw from.
3. Defrag showed a permanent `4×RUN` on a genuinely idle box because its
   gate (`aiclk >= 200 && power > 8W`) is permanently true for idle
   Blackhole. Fixed to use `AdaptiveBaseline`-relative signals — which
   surfaced two more adjacent bugs: the idle-baseline capture arm had no
   guard so it never stabilized, and the Running-transition capture was
   overwriting a correctly-learned rest baseline with the loaded reading
   (undoing the actual Phase 29 fix). The regression test guarding that
   Phase 29 fix had itself encoded the bug as a setup assumption (asserted
   `Running` for a 9W sample — exactly the defect being repaired here) and
   had to be moved off that phase-gate onto a dedicated test. Lesson:
   changing a state machine's transitions can silently orphan an existing
   regression guard that only reached its subject through that transition.

## Phase 35–36: Docker vLLM detection fix, Arcade cleanup (Sep 2, 2026, v0.13.3–0.13.4)

`DockerProbe` hard-gated on container image-name substrings before ever
calling `docker inspect`, so tt-model-manager's `tt-model/<name>:<hash>`
image tags (a real live container, confirmed via `docker inspect`) were
filtered out regardless of what was running inside. Fixed by accepting a
container when its image matches a known TT pattern **or** its
Entrypoint+Cmd is vLLM-shaped **and** its `HostConfig` actually maps
`/dev/tenstorrent` — hardware-verified live and user-confirmed in the TUI.
Separately: Memory Castle's standalone-mode legend footer was the one
element in that file never gated behind the `chrome` flag Arcade uses to
suppress duplicate/embedded UI text, so it leaked into the composited
Arcade view — fixed. A labeling audit found six different device-naming
conventions in concurrent use across views (`BH0`, `Dev0`, `D{n}`, `Device
N: BH`, bare numerals); added `Device::short_label()` (`"BH0"`) and adopted
it wherever there was width budget for it (Arcade's telemetry strip, Memory
Castle headers, the fleet-grid row label, the standalone title bar) —
deliberately left the TRON/32+-chip fleet-grid bare numeral and
HivemindSweeper's `D{d}` columns alone, since both are purpose-built for a
minimal per-cell width budget. Also caught in passing: Memory Castle's
Compact-tier header was still labeling by loop position instead of
`device.index` — the same sparse-index bug class documented elsewhere.

## v0.13.5 (Sep 9, 2026)

Inference detection missed a tt-model-manager-launched diffusion (SkyReels)
container, since it isn't vLLM-shaped and its `tt-model/` image tag matched
no known pattern — recognized as its own signal, gated on the container's
real `/dev/tenstorrent` mapping (never trusted on image tag alone, since
`tt-model/` is a broad namespace). Even once detected, the panel showed only
CPU/RSS: the kernel/weight-load progress probes never searched
`$TT_METAL_CACHE/tt-metal-cache` or `$TT_DIT_CACHE_DIR` — the actual roots
tt-metal and every diffusion/DiT model family use — so they always reported
zero hits. Both env vars added to `host_exec`'s forwarded-variable
allowlist.

## v0.13.6 (Sep 23, 2026)

Defrag TUI's model-loading visualization went dark on p300c (dual-ASIC)
under confirmed heavy load: every phase-transition gate requires `power >
POWER_IDLE_W`, and per-ASIC `Telemetry::power` (TDP register) can read
exactly 0.0 W independently of the rest of the board — live `tt-smi -s`
showed `power`/`VCORE` zeroed while `board_power` (~288 W), current
(50-64 A), aiclk (1350 MHz boosted) and ASIC temp (65-70°C) all said the
device was running. Added `effective_power_w()`: falls back to
`board_power` only when per-ASIC `power` is absent/near-zero and
`board_power` itself clears `POWER_IDLE_W`; leaves single-ASIC/healthy
boards untouched and doesn't fabricate load on a genuinely idle card.
Render-path power readout (`render_power_bar`) untouched — this was
specifically the animation-liveness gating going dark, not the numeric
display.

## v0.13.7 (Sep 23, 2026)

Training view's network sweep (`src/animation/train_view.rs`) was purely
decorative: it advanced on a fixed wall-clock rate
(`SWEEP_SUBCOLS_PER_SEC`), so a stalled trainer animated identically to a
healthy one — violating the tool's own every-pixel-is-real-signal premise.
Replaced with a step-gated sweep: it starts when `TrainState.step` actually
changes (a real log event) and traverses over that step's measured
`step_ms`, then holds at rest — no perpetual loop — until the next real
step lands; no lit pulse before a first step/timing is observed. Added pure
`step_progress(elapsed_secs, step_ms)` (unit-tested) and `Cell`-based
`last_step`/`step_started_at` on `TrainView` to detect the step boundary
from `&self`. Removed the now-dead `sweep_head`/`SWEEP_SUBCOLS_PER_SEC`;
`sweep_at`'s glow/falloff shape is unchanged, just fed a step-derived head
instead of a wall-clock one. Mock runs (`MockTrainRun`) needed no special
casing — they already derive `step`/`step_ms` at the same real cadence a
live run would.
