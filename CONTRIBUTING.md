# Contributing to yoetz

Thank you for your interest in contributing to yoetz!

## Development Setup

### Prerequisites

- Rust 1.88+ (check with `rustc --version`)
- Git

### Getting Started

```bash
# Clone the repository
git clone https://github.com/avivsinai/yoetz.git
cd yoetz

# Build the project
cargo build

# Run tests
cargo test

# Run clippy
cargo clippy

# Format code
cargo fmt
```

## Project Structure

```
yoetz/
├── crates/
│   ├── yoetz-core/     # Core library (types, bundling, config)
│   └── yoetz-cli/      # CLI binary
├── recipes/            # Browser automation recipes
├── docs/               # Documentation
└── skills/             # Agent skill definitions
```

## Code Style

- Follow Rust conventions and idioms
- Run `cargo fmt` before committing
- Ensure `cargo clippy` passes without warnings
- Write tests for new functionality

## Pull Request Process

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Make your changes
4. Run tests and linting (`cargo test && cargo clippy`)
5. Commit with a descriptive message
6. Push to your fork
7. Open a Pull Request

## Release Process

- Keep feature PRs feature-only. Do not edit `Cargo.toml`, `Cargo.lock`, or `CHANGELOG.md`
  in ordinary feature branches.
- Prepare releases in a dedicated `release/vX.Y.Z` branch or PR from `main`.
- In the release PR:
  - bump the workspace version in `Cargo.toml`
  - update `Cargo.lock`
  - regenerate `CHANGELOG.md` with `git-cliff --tag vX.Y.Z -o CHANGELOG.md`
- Merge the release PR, then tag that merge commit as `vX.Y.Z` to trigger the release workflow.
- CI only validates the changelog when release metadata changes, so feature PRs are not
  blocked on release prep.

## Commit Messages

Use clear, descriptive commit messages:

- `feat: add vision support for Gemini`
- `fix: handle UTF-8 truncation correctly`
- `docs: update README with examples`
- `refactor: extract provider module`
- `test: add integration tests for council`

## Reporting Issues

When reporting issues, please include:

- Rust version (`rustc --version`)
- Operating system
- Steps to reproduce
- Expected vs actual behavior
- Relevant error messages

## License

By contributing, you agree that your contributions will be licensed under the MIT License.
