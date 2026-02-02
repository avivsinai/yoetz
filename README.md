# yoetz

Fast, CLI-first LLM council + bundler + browser fallback for coding agents.

## Status

Phase 4 in progress: council aggregation + apply/patch.

## Quick start

```bash
# Bundle only
cargo run -p yoetz -- bundle --prompt "Review this" --files "src/**/*.rs" --format json

# Sync registry (OpenRouter + LiteLLM)
cargo run -p yoetz -- models sync --format json

# Ask (requires provider + model in config)
cargo run -p yoetz -- ask --prompt "Review this" --files "src/**/*.rs" --format json

# Council (multi-model)
cargo run -p yoetz -- council --prompt "Review this" --models openai/gpt-4o,anthropic/claude-3.5-sonnet

# Apply patch
yoetz apply --patch-file /tmp/patch.diff
```

## Roadmap (high level)

- Skills polish for Claude Code + Codex CLI
- agent-browser recipe expansion (ChatGPT/Claude/Gemini flows)
```
