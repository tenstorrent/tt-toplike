<!-- Thanks for contributing to tt-toplike! Fill in the summary and tick the checklist. -->

## Summary

<!-- What does this PR change, and why? Link any related issue (e.g. "Closes #123"). -->

## How was it tested?

<!-- Commands you ran, backends/hardware you tried (--mock / --host / real TT / --remote), screenshots or asciicasts for visual changes. -->

## Checklist

- [ ] `cargo fmt` — code is formatted
- [ ] `cargo clippy --locked --lib --bin tt-toplike-tui --features tui -- -D warnings` — no warnings
- [ ] `cargo test --locked --lib --features tui` — tests pass
- [ ] New source files carry the SPDX header (`// SPDX-License-Identifier: Apache-2.0` + `// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.`)
- [ ] Docs updated if behavior changed (README / QUICK_START / docs/)
- [ ] Version files bumped if this is a release (`Cargo.toml` + `debian/changelog`)
