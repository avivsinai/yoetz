# Contributing to yoetz

Thank you for your interest in contributing to yoetz!

## Development Setup

### Prerequisites

- Rust 1.88+ (check with `rustc --version`)
- Git
- Node.js for ChatGPT native-extension script tests
- Browser helper binaries only when working on browser transports:
  `chrome-devtools-mcp`, `dev-browser`, or `agent-browser`

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

# Optional: verify the ChatGPT native extension package and JS tests
./scripts/build-chatgpt-native-extension.sh --check
node --test extensions/chatgpt-native/tests/*.test.js
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
- Run `cargo fmt --all -- --check` before committing
- Ensure `cargo clippy --workspace --all-targets -- -D warnings` passes
- Write tests for new functionality
- Keep `vendor/headless_chrome` changes separate and intentional; it is patched
  into the workspace through `[patch.crates-io]`.

## Pull Request Process

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Make your changes
4. Run tests and linting (`cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`)
5. Commit with a descriptive message
6. Push to your fork
7. Open a Pull Request

CI also runs formatting, MSRV 1.88, `cargo deny`, gitleaks, browser script
checks, and a real-browser smoke job. If your change touches release behavior,
extension packaging, browser transports, or dependency metadata, call that out
in the PR.

## Release Process

Releases are cut from `main` only with `./scripts/release.sh X.Y.Z` and the
resulting release PR. The merged release commit drives the tag, GitHub release
artifacts, Homebrew formula, Scoop manifest, and skill marketplace publication.
Do not create manual tags or GitHub releases for normal releases.

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
