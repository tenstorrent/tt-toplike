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
3. **Test your changes** - ensure `cargo test` passes and `cargo build --all-features` succeeds
4. **Update documentation** if you're adding new features or changing behavior
5. **Commit your changes** with clear, descriptive commit messages
6. **Push to your fork** and submit a pull request to the `main` branch

### Pull Request Review Process

- Pull requests are typically reviewed **weekly**
- Maintainers will provide feedback on your submission
- Once approved, your PR will be merged by a maintainer

## Development Setup

### Prerequisites

- Rust 1.75 or later
- Cargo
- For Debian packaging: `debhelper`, `devscripts`

### Building

```bash
# Build TUI only (safe defaults)
cargo build --release --bin tt-toplike-tui --features tui,json-backend,linux-procfs

# Build with all features
cargo build --release --all-features

# Run tests
cargo test
```

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
- Ensure existing tests pass: `cargo test`
- Test with the mock backend: `cargo run -- --mock --mock-devices 4`

## Code of Conduct

This project adheres to the [Contributor Covenant Code of Conduct](CODE_OF_CONDUCT.md). By participating, you are expected to uphold this code. Please report unacceptable behavior to ospo@tenstorrent.com.

## Questions?

If you have questions about contributing, feel free to:

- Open a [GitHub Issue](https://github.com/tenstorrent/tt-toplike/issues)
- Contact the maintainers at ospo@tenstorrent.com

## License

By contributing to tt-toplike, you agree that your contributions will be licensed under the [Apache License 2.0](LICENSE).
