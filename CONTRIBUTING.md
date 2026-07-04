# Contributing to tt-toplike

Thank you for your interest in contributing to tt-toplike! We welcome contributions from the community.

## How to Contribute

### Reporting Bugs

If you find a bug, please report it using [GitHub Issues](https://github.com/tenstorrent/tt-toplike/issues). When reporting a bug, please include:

- A clear and descriptive title
- Steps to reproduce the issue
- Expected behavior vs. actual behavior
- Your environment (OS, Rust version, hardware if relevant)
- Any relevant logs or error messages

### Suggesting Features

We welcome feature suggestions! Please open a [GitHub Issue](https://github.com/tenstorrent/tt-toplike/issues) with:

- A clear description of the feature
- The use case or problem it solves
- Any implementation ideas you may have

### Submitting Pull Requests

1. **Fork the repository** and create your branch from `main`
2. **Make your changes** following the project's coding standards
3. **Test your changes** - ensure `cargo test` passes and the build succeeds (see **Building** below)
4. **Update documentation** if you're adding new features or changing behavior
5. **Commit your changes** with clear, descriptive commit messages
6. **Push to your fork** and submit a pull request to the `main` branch

### Pull Request Review Process

- Pull requests are typically reviewed **weekly**
- Maintainers will provide feedback on your submission
- Once approved, your PR will be merged by a maintainer

## Development Setup

### Prerequisites

- **Rust toolchain** — pinned by `rust-toolchain.toml` (channel `1.93.0`, with `rustfmt` + `clippy`). If you use `rustup`, it auto-installs and selects that toolchain the first time you build in the repo, so you don't need to manage it by hand. (The exact minimum supported version below 1.93.0 hasn't been established — treat the pinned toolchain as the supported floor.)
- Cargo
- For Debian packaging: `debhelper`, `devscripts`

### Before you push — the CI gates

CI runs exactly these three checks. Run them locally and you'll match the pipeline:

```bash
# Tests (default feature set)
cargo test --locked --lib --features tui

# Clippy — warnings are hard errors
cargo clippy --locked --lib --bin tt-toplike-tui --features tui -- -D warnings

# Formatting — CI only checks PR-changed files, but formatting everything is safe
cargo fmt
```

CI additionally runs the test suite once with the minimal feature set
(`--no-default-features --features tui,json-backend`) to keep the `cfg`-gated
paths honest.

### Building

```bash
# TUI (safe defaults — no Luwen, no GUI, runs without hardware)
cargo build --release --bin tt-toplike-tui --features tui,json-backend,linux-procfs

# GUI (requires a desktop environment with Vulkan/GL)
cargo build --release --bin tt-toplike-gui --features gui

# Luwen backend (direct PCI access — requires ttkmd kernel module loaded)
# Only add this if you specifically need Luwen; it is NOT included by default
cargo build --release --bin tt-toplike-tui --features tui,json-backend,linux-procfs,luwen-backend

# Run tests
cargo test --lib --features tui
```

> **Note**: `--all-features` will not work because several features are mutually
> exclusive or require hardware that is unavailable in a build environment.

### Repository map

Where things live, so you know which directory to reach for:

- **`src/animation/`** — the visualizations. Starfield, Memory Castle, Memory Flow, Defrag, Arcade (+ the `⚔` `duel`), and the `[i]` Inference Server Monitor's `snake` (which composes `model_starfield` for the cold roam, `inference_load` for the loading journey, and `serving_creature`/`serving_panels` for the live dashboard). Pure rendering + telemetry-driven state — no I/O.
- **`src/backend/`** — telemetry sources behind the `TelemetryBackend` trait: `sysfs` (hwmon), `json` (`tt-smi -s`), `hybrid` (sysfs + background JSON), `host` (CPU/RAM/GPU/ANE), `mock`, `ws` (remote), and `luwen`. **Safety note:** `luwen` does direct PCI BAR0 access and can disrupt a running workload — `factory.rs` deliberately keeps it out of auto-detect (it is reachable only via explicit `--backend luwen`, or by stepping onto it in the `b` backend cycle).
- **`src/workload/`** — process + inference detection: `process_monitor`/`host_processes` enumerate processes, `inference_match` tags known runtimes, and **`inference_server/`** probes local model servers (Docker/HTTP detection, vLLM `/metrics`, readiness/liveness) that feed the `[i]` snake. `model_catalog` maintains the bundled + background-refreshed compatibility catalog.
- **`src/ui/tui/`** — the TUI rendering and event loop (`mod.rs` owns key handling, the command bar, and the mode/overlay state machine); `chip_portrait`, `inference_panel`, `perf`, `throttle`, `bench` are its helpers.

### Local helper scripts

These are convenience scripts for local development, not part of CI:

- `test-modes.sh` / `test-egui.sh` — quick manual smoke-runs of the visualizations / GUI
- `record-casts.sh` — regenerates the `assets/casts/*.cast` asciinema recordings
- `build-deb.sh` — full `.deb` build (see the vendoring note below)

### Vendored dependencies

`vendor/` is **not committed** (it was through v0.7.18, but at ~1.1 GB / 35k
files it made clones hostile — it's now in `.gitignore`). `build-deb.sh`
regenerates it on demand via `cargo vendor` so the Debian package builds fully
offline; `build-deb.sh --quick` reuses a `vendor/` that's already present.
Normal `cargo build`/`cargo test` don't need it at all.

### Code Style

- Follow standard Rust formatting conventions (`cargo fmt`)
- Run `cargo clippy` and address any warnings
- Add SPDX headers to all new source files:
  ```rust
  // SPDX-License-Identifier: Apache-2.0
  // SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.
  ```

### Testing

- Write unit tests for new functionality
- Ensure existing tests pass: `cargo test --locked --lib --features tui`
- Test with the mock backend: `cargo run -- --mock --mock-devices 4`

## Code of Conduct

This project adheres to the [Contributor Covenant Code of Conduct](CODE_OF_CONDUCT.md). By participating, you are expected to uphold this code. Please report unacceptable behavior to ospo@tenstorrent.com.

## Questions?

If you have questions about contributing, feel free to:

- Open a [GitHub Issue](https://github.com/tenstorrent/tt-toplike/issues)
- Contact the maintainers at ospo@tenstorrent.com

## License

By contributing to tt-toplike, you agree that your contributions will be licensed under the [Apache License 2.0](LICENSE).
