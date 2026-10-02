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

## Reset takeover animations (Sep 28, 2026)

Prompt: "when we detect a `tt-smi -r` has been run... show a full screen
takeover animation" — a BBS sysop interrupt, a 1024-Blackholes swarm, a Lost
hatch countdown, Missile Command, a classic Trek reset screen, or (weighted
toward subset resets) a quiet notification, chosen at random and scoped by
whether the reset targets all chips or a subset. It began as an opt-in
flag (`--reset-takeover`), since replaced by `--tt-smi-reset-behavior` (see
the entry below). HivemindSweeper gets a distinct
treatment instead of a takeover: the reset is injected as a real feed event
(`EventKind::Reset`) rather than an interruption of the one view whose whole
purpose is watching real signals.

Detection reuses the TUI's existing per-tick process scan (no new polling
thread) and tracks the real `tt-smi -r` process's actual lifetime — "in
progress"/"done" are never a fixed fake timer, only real pid liveness.
Animation variants are six concrete structs behind a plain enum (matching
this codebase's `DisplayMode`/`EventKind` convention over `dyn Trait`), each
embedding a shared `TakeoverClock` for lifecycle bookkeeping.

Independent task review (13-task subagent-driven build) caught two real bugs
before merge: `is_full`'s boundary check was a length comparison, so a
duplicated target (`tt-smi -r 0 0` on a 2-chip box) falsely read as a full
reset — fixed with a `HashSet`-based coverage check. Separately,
`ResetDetector::clear()` was called while the real process was often still
alive (by design, right after a HivemindSweeper injection, and on any
skip-key dismissal), so the same reset kept getting re-detected every scan —
fixed by remembering the last-handled pid across `clear()`.

The final whole-branch review then found three more, all fixed: `sysinfo`'s
`ProcessRefreshKind::everything()` enumerates threads alongside processes,
so the detector's naive first-match scan could latch onto a worker thread's
tid instead of the real process (reproduced without hardware — a fake
multithreaded stand-in plus a probe using this crate's exact sysinfo calls —
fixed by filtering `thread_kind().is_none()`); the BBS variant labeled chips
by loop position instead of the real targeted indices and claimed "reset ack
received" while still in progress, violating the "never claim a specific
chip completed when unknown" rule from the design spec; and HivemindSweeper's
feed never got the spec-mandated distinct visual treatment for a reset event
(no renderer anywhere matched on `EventKind::Reset`) — added a sticky
`is_reset` flag threaded from `SniffEvent` through `FeedAgg` into a
magenta-accented feed row, additive to the existing severity coloring.

Hardware-verified live on 4× Blackhole (p300c): the real `tt-smi -r`
invocation is a python-shebang script run via its venv interpreter
(`<venv>/bin/python <path>/tt-smi -r <targets>`), not a bare binary — the
parser's process-`comm`-based matching handles this correctly without
needing its argv[0] fallback path. Real single-chip and full-box resets both
took ~41-43 seconds, comfortably clearing the 2-second detection-scan
cadence (an earlier un-timed spot check had wrongly suggested resets might
complete in under a second). End-to-end runs of `tt-toplike-tui
--reset-takeover` through both a real single-chip and a real full-box reset
produced no panic. The rendered animation content itself (which variant
appeared, glyph layout) could not be visually confirmed — ratatui's raw-mode
full-screen rendering isn't inspectable through a non-interactive shell, a
known limitation of this kind of automated verification, not a defect.

The design spec's optional kmsg log-line enrichment (real per-chip tt-kmd
text when `/dev/kmsg` is readable) was deliberately not built — every
variant uses only the honest generic-fallback pacing described as the
spec's fallback path. Flagged explicitly rather than silently dropped; a
candidate follow-up, not a gap.

### Follow-up polish pass (Sep 29, 2026)

Live feedback after using the feature: Missile Command's original blinking
bracket-strip had no real motion and a flat red tint that read as harsh —
rebuilt as a per-lane missile that actually descends over real elapsed
time toward each targeted chip, blooms into a fading burst ring on impact,
staggered per lane, over a deep steel-blue night-sky wash instead of red.
Trek's single sensor-scan row became the real "Super Star Trek" (1971
BASIC) screen: a status sidebar (STARDATE/CONDITION/KLINGONS REMAINING
tied to real reset state) plus a fixed 8x8 sensor grid, so it reads as
walking in on a game already in progress rather than a title card.
Separately: takeovers now reveal the real screen beneath them through a
55%-blend color filter (a `TintOverlay` widget replacing the old `Clear`)
instead of blacking it out — Block/Paragraph only patch the style fields
they explicitly set, so untouched cells keep showing (tinted) real content.

BBS's rainbow hue-cycling turned out to be too much — toned down to a
fixed classic-BBS palette, an ANSI box border, modem-connect flavor lines,
and a real typewriter reveal per chip line, with the "psychedelic"
treatment kept specific to BBS rather than spreading everywhere. Quiet
Notice, Blackhole Swarm, and Hatch Countdown were brought up to the same
bar: Quiet Notice got a shaded ANSI block backdrop (this app's own
`BLOCK_CHARS`/`hsv_to_grayskull` vocabulary, plus a 4x4 Bayer ordered
dither so nearby cells actually spread across the full glyph ramp instead
of clustering on one shade) that shimmers gently; Blackhole Swarm — never
read the clock at all before this — now has each glyph twinkling
independently; Hatch Countdown gained a real seven-segment LED digit
display and the show's own numbers as an easter egg.

Added a 7th variant, Fail Whale: a flock of birds (count scales with the
real chip count) carries the whale on ropes, hovering and wing-flapping
while the real reset is still in progress — never faking a landing time —
then gliding into a soft touchdown once it's actually finished, birds
flying off happily. Caught during its own build: a sub-one-row bob
amplitude computed a technically-changing position that rendered as a
frozen scene, since terminal cells only have integer rows (fixed by
widening the amplitude and testing the position calculation directly
rather than a rendered snapshot); and a "landed" ground-row position
computed from the bottom of the screen alone pushed the whale's own lower
body and the ground line clean off the bottom of the terminal (fixed by
reserving room for the scene's real height below that point).

### Takeover box (Oct 1, 2026)

Prompt: "for the overlays on tt-smi -r takeovers. let's make them all the same
'size' centered in the screen. We show the filtered overlay behind this box,
but the box itself is always the default terminal background color, and we
show our focused art for tt-smi -r right there."

All seven variants now draw in one box from `takeover_box(area)` in
`src/animation/takeover/mod.rs`: 72 columns by 24 rows, centered, shrinking to
keep at least 2 columns and 1 row of margin on a smaller terminal. The color
wash still covers the whole screen. The box is cleared first, so inside it the
background is the terminal default. Border is left and bottom only. The usable
interior (`takeover_interior`) is 71x22 at full size, because ratatui gives
the title its own row. Quiet Notice, Blackhole Swarm, Missile Command and Fail
Whale size their art from the interior. BBS shows only the newest chip lines
once the list would pass 22 rows. Trek (36x20) and Hatch (17x9) already fit.

### `--tt-smi-reset-behavior` (Oct 1, 2026)

Prompt: "I feel like the --command for the tt-smi reset isn't obvious. can we
get a --tt-smi-reset-behavior setting instead. `ignore` or `inform` or
`dazzle`. inform incorporates it into the status bar of each view. inform is
default. dazzle is what we've now made more or less. and then there's also
`demo`..."

`--reset-takeover` and its config key are gone (never released). The setting
is `--tt-smi-reset-behavior <ignore|inform|dazzle|demo>` (config key
`tt_smi_reset_behavior`, flag wins, default `inform`; `ResetBehavior` and
`resolve_reset_behavior` in `src/cli.rs`). `ignore` never runs the detector.
`inform` and up show a status-bar segment in every view: `⟳ tt-smi -r ·
{scope} · resetting`, then `✓ tt-smi -r done` for 10 seconds. `dazzle` adds
the takeover. `demo` plays all seven takeovers in a fixed order (Quiet
Notice, Blackhole Swarm, Hatch Countdown, BBS, Trek, Fail Whale, Missile
Command) at boot and again on every real reset (Oct 1, 2026, Task 2).
`Takeover::Demo(DemoSequence)` is in `src/animation/takeover/demo.rs`. Each
animation gets an 8 s slot (`SLOT_LEN`) and is told its reset finished at
5 s (`RESOLVE_AT`). The real reset's finish is ignored on purpose, so the
sequence always runs to the end. That is a deliberate exception to
the "follow the real reset lifecycle" rule, and the box title carries
`DEMO - {title}` (boot) or `DEMO (real reset) - {title}` so a staged
animation is never mistaken for a real one. The tag goes through
`render_takeover_frame`'s `tag` argument. Esc and q/Q end a demo; any other
key skips one animation; non-demo takeovers still treat every key as a skip.
Key routing, boot gating and takeover choice are the pure functions in
`reset_status.rs` (`demo_key_action`, `should_start_boot_demo`,
`boot_takeover`, `takeover_for_event`, `replace_takeover`). The boot demo
uses a synthetic full reset over the real device count and is skipped in
HivemindSweeper.
State and per-scan decisions live in `src/ui/tui/reset_status.rs`. The
segment follows the real process itself, so a skipped takeover does not
stop it reaching "done" (see the fix-wave entry below). The status bar now
drops whole hotkey groups (then hint groups) from the right to fit beside
the chip telemetry. The old bar was one clipped paragraph, so on a narrow
terminal it cut off the chip telemetry at the right edge; now the
telemetry stays and hotkeys go first.

### Reset-behavior fix wave (Oct 1, 2026)

Review minors from the two tasks above, fixed in one pass.
- `ResetStatus` now follows every live `tt-smi -r` process by itself (pid
  and cmdline together, pruned each scan). Before, it was replaced only when
  the single-slot `ResetDetector` reported a new reset. Two overlapping
  resets made the segment flip between them every 2 s and sent a
  HivemindSweeper feed event every 2 s. A reset during a 56 s demo got no
  segment at all. Now each distinct reset is reported once, the segment
  shows the newest live reset's scope, and `done` starts when the last one
  ends. `ResetDetector` only holds the reset a takeover is for
  (`begin`/`is_finished`/`clear`), so a second reset still gets no second
  takeover.
- `scan_resets` takes the process snapshot as a closure and calls it only
  when the behavior detects resets, so the loop has one tested gate.
- `boot_takeover` does not start the boot demo on a terminal under 8x4
  (`takeover_fits`). Before, the demo played unseen there and swallowed
  keys for up to 56 s.
- `--help` lists each value once, from the `ResetBehavior` doc comments.
- A demo test renders the Missile Command slot for chips 1 and 3 of 4 and
  checks which lanes are lit. The Task 2 report said this was covered by
  Missile's own tests. It was not: those only check that rendering does
  not panic.

## Phase 37: Training tapestry (Oct 1, 2026, v0.13.6)

Prompt: the Training View's top-left block-head scanner was "not the right
usage of space or signal"; asked for three alternatives rooted in hardware
and training performance data. Chosen: all of them together, with the old
grid kept as a backdrop.

Finding that drove the design: the grid's sweep was driven by the frame
counter and every node took the same loss hue, so none of it was data, and
tt-train logs no per-block or per-head signal. Any per-node mapping would
have been invented.

Decisions: per-step history only from trainer-reported times (derived
cadence is an average and would fake resolution); "chip" in gauges and the
verdict means the busiest chip because the trainer's chips are not
identified; MockBackend left alone (its telemetry feeds the Insights
sidebar tests), so --mock has no PCIe gauge; slope is per 100 logged
losses; pulse capped at 5 passes/s so it stays visible.

Finding that drove the bar-timing work: the first real run showed `no
per-step times reported`, because tt-tnt prints only a tqdm bar per
3195-step chunk. Step times are now observed from progress-bar updates
(gap between polls that saw consecutive steps, only when exactly one step
was seen, never across a bar restart or from an unparsed step 0). The
title reads `STEP ANATOMY (from bar)`. The same run exposed a bug: derived
step rate and tokens/sec froze after a bar restart; fixed. The monitor's
per-run anchors reset at attach and detach. Narrow widths show whole
clauses only.

Follow-up: the tt-tnt harness change on branch `dazzle-me/tt-train` in the
tt-tnt repo now prints one `Step: N, Loss: L, Time: T ms` line per step.
tt-toplike reads it as a trainer-reported time (`StepAndMs`) with unknown
cache growth. The LIVE cache row shows only when a cache count was reported.

### Resumed runs: `step x / y` with y below x (Oct 2, 2026)

The user's log holds two runs in one file. Run 2's header says `steps=25560`
(the steps that process runs) while its val lines carry absolute steps
(38340 and up), so the header drew `step 38,340 / 25,560`. Three changes:
`parse.rs` reads `resumed from ... at step S ...; running M more steps to
step E` as `TrainEvent::Resumed` (E, or S + M when `to step` is missing, and
`None` when neither is readable); the state takes E as the absolute budget and
records `resume_start`. A run header that follows run data calls
`TrainState::begin_new_run`, and `poll` clears the monitor's step anchor and
reported-time flag at the same point. `draw_header` and the convergence strip
treat a step past its budget like a chunk-local step. Version not bumped here.

Process: brainstorming -> spec (docs/superpowers/specs/2026-10-01-
training-tapestry-design.md) -> plan (docs/superpowers/plans/2026-10-01-
training-tapestry.md).

### Starfield over a signal weave (Oct 2, 2026, v0.13.7)

Prompt: "the pulse doesn't make efficient use of the space the way the old
knots / stars did. rethink that area of the viz again". Chosen: "Starfield
over a signal weave".

The step bars, the pulse row, the node-grid backdrop and the four gauge rows
are gone. The band now draws a braille starfield (one star per step, two
steps per column, up to 6 rows, `◆` compile, `✺` checkpoint, a dotted median
horizon, the newest three stars swelling once per measured step) over weave
rows on the same columns: chip power, the busiest chip's aiclk, PCIe and host
CPU, each ending in its current value. Pure geometry lives in
`train_canvas.rs`; `plan_band` grants star, chip, aux, verdict and strip rows.

Decisions: the value column (10 wide) appears only when 20 data columns
remain, and a value that does not fit is left out whole; a weave reading of
zero draws `▁` so a sampled column is never blank; with no step samples the
aux rows keep their label and value and get no cells; the tokens/sec best was
removed with its gauge (tok/s stays in LIVE); `--mock` gained a closed-form
host CPU and RSS so the host row shows in screenshots, and still has no PCIe
row. The band stays capped at 13 rows, so with three chips and all aux rows
the starfield gets 4 rows with a verdict (5 without); it reaches 6 with fewer
weave rows.

Row order, decided after review: `plan_band` grants one star row, the
verdict, chip rows, star rows up to 3, aux rows, the strip, then star rows up
to 6. The first order put the aux rows and the strip ahead of the second star
row, so a 30-row terminal (a 10-row band) with three chips and two aux rows
left the starfield 2 rows. It now keeps 3 and drops the strip. Raising the
13-row cap was not done, because it would take rows from the river. Star
rows are granted only when a shown step has a time the canvas can place, so
a history of zero times leaves no blank star rows; its chip rows still draw.

Notable: a deliberate break that drew a PCIe row with no counters passed the
first weave test, because it only checked the rows it expected. The test now
asserts that a row with no signal is absent.

### Step jump timed from the attach baseline (Oct 2, 2026)

Bug: the Training view showed 630,111 tok/s against a true rate near 110k.
The tt-tnt harness prints no per-step times and its bars reach the log only
when a chunk ends, so the only step signal is the validation line every 3195
steps. `note_step_progress` timed the first jump from the attach poll. The
viewer had attached 166 s into the chunk, so 3195 steps over 166 s gave 52
ms/step. A per-step trainer had the same flaw on a smaller scale: its first
step after attach was timed from the attach poll.

Fix: `TrainMonitor.anchor_is_baseline` marks the anchor set by the first step
seen after attach or a new-run reset. The first increase after a baseline
re-anchors as an observed change and records no `step_ms` and no observed
sample. A measurement needs two observed changes. A bar restart (the
regression arm) anchors as an observed change, because the restart is an
event with a known time; the old test's intent (timing resumes at once)
stays. `reset_run_anchors` and the new-run header clear the flag.

Consequences: a per-step trainer loses one step of cadence after attach. A
chunk-jump trainer gets its first `step_ms` at the second chunk end after
attach, and that value spans train, checkpoint and validation time, so it is
an effective throughput a little below pure training speed. Until then
`step_ms` is 0, `tokens_per_sec()` is `None` and the LIVE panel shows no
tok/s. That is the true state: nothing has been measured yet.

Tests: `baseline_anchor_tests` in `monitor.rs` (the reported 60711 to 63906
case, a per-step trainer, bar restart, new run, reported time, and a sweep
of jump sizes and gaps, and a `poll` test for the new-header branch).
Eight existing tests that measured from the attach baseline gained one more
step before their first measurement: the cadence derivation test, the
one-step sample, the several-steps poll, the bar restart (two copies), the
implausible gap, the fresh run after a previous one, the unparsed step zero
and the resume start step. Version not bumped.

Follow-up (Oct 2, 2026): the monitor is polled only while the Training view
is on screen, so a stale anchor could still time a chunk jump after the user
returned (a 13 minute chunk seen 6 s after the return would show about 17M
tok/s). `note_step_progress` now remembers `last_note_at`. A gap over
`POLL_GAP_REBASE` (5 s) makes that call a re-baseline: it anchors at the
current step, measures nothing and records no sample. Returning to the view
therefore costs one more observed change before a rate appears. The monitor
still does not poll in other views. The backlog test and the two reported-time
tests had become vacuous under the baseline rule and each gained an anchoring
step so they fail again when the lower bound or the early return is removed.
Tests that jump minutes of injected time use `keep_polling`, so they do not
look like a pause.

### Process scan off the render thread (Oct 2, 2026, v0.13.8)

Report: "this branch seems to have a systematic lag again. where you can
really see the polling shift every 2 seconds or so". The user ran
`--tt-smi-reset-behavior demo`, where smooth animation makes a stall easy to
see.

Measurement (548 processes, 2,909 threads, release build): the 2 s block in
`run_app` cost about 135 ms on the render and input thread. Most of it was
`HostProcessMonitor::update` (30 ms, sysinfo with every process field) and
`ProcessMonitor::update` (82 ms, the `/proc` fd and hugepage scan). Building
rows and lists added about 22 ms. `InferenceServerProbe::update` makes
network probes with a 150 ms timeout each. That is about eight dropped frames
at 60 fps. Rendering the Training view costs 0.4 ms, so rendering was not the
cause. The block is the same on `origin/main`.

Design: `src/ui/tui/proc_scan.rs`. `Scanner` owns the three monitors and does
what the block did, in the same order, with the same `cfg` variants.
`ScannerHandle` runs it on a worker thread named `tt-toplike-proc-scan`.
`request` and `try_result` never block. At most one request is outstanding,
so a slow scan never queues a second one. The worker catches a panic in a
scan, logs it and keeps serving. The TUI panic hook returns early on that
thread, so a caught panic leaves the terminal in the alternate screen. Drop
waits up to 250 ms for the worker and then detaches it.

Loop: setup runs one synchronous scan so the first frame has rows. Every
iteration checks `try_result` and, when a result is ready,
`apply_scan_result` swaps in `proc_rows` and `serving_metrics`. The loop then
runs the reset scan over the result's process list, submits runtimes and
inference servers, checks the Defrag unload edge and runs the `/serve`
publisher step. The 2 s cadence block only sends a request, then refreshes
host CPU and memory as before. `ScanRequest.want_processes` comes from
`reset_behavior.detects()`, so `ignore` still never takes the process
snapshot.

After (release, same box, 274 processes at the time): `request` about 5 us,
`try_result` about 1 us, apply plus reset scan about 45 us. Before, measured
in the same run: 131 to 135 ms per refresh. The cost of the move is one scan
of latency: results appear about 0.15 s after the request.

Tests: `proc_scan::tests`, with a fake scanner that sleeps and can panic, the
apply step as a pure function, the real scanner on this machine, and a source
guard that `run_app` reaches the monitors only through the handle. Each
wiring test was seen to fail under a deliberate break: a blocking `request`,
a blocking `try_result`, no outstanding gate, an unbounded join on drop, no
panic catch, an unnamed thread, an apply that keeps old rows or drops the
process list, an inline scan in the loop, and `want_processes: true`.
