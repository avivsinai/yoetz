# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in yoetz, please report it responsibly.

**Do not open a public GitHub issue for security vulnerabilities.**

Instead, please email: **aviv@sinai.dev**

### What to Include

- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

### Response Timeline

- **Acknowledgment**: Within 48 hours
- **Assessment**: Within 1 week
- **Fix**: Depending on severity, typically within 2 weeks

## Scope

This policy covers the yoetz CLI tool and its yoetz-core library. Issues with upstream LLM provider APIs should be reported to those providers directly.

## Security Measures

This project uses:

- **gitleaks** - Automated secret scanning in CI
- **cargo-deny** - Dependency vulnerability and license auditing
- **clippy** - Static analysis for common Rust pitfalls
- No `unsafe` code in library
