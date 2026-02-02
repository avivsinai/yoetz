# yoetz

Fast, CLI-first LLM council + bundler + browser fallback for coding agents.

## Status

Phase 5 in progress: review flows + expanded browser recipes.

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

# Review current git diff
cargo run -p yoetz -- review diff --model openai/gpt-4o

# Review a single file
cargo run -p yoetz -- review file --path src/main.rs --model openai/gpt-4o

# Apply patch
yoetz apply --patch-file /tmp/patch.diff

# Browser passthrough (agent-browser must be installed)
cargo run -p yoetz -- browser exec -- open https://chatgpt.com/

# Run a recipe
cargo run -p yoetz -- browser recipe --recipe recipes/chatgpt.yaml --bundle /path/to/bundle.md
cargo run -p yoetz -- browser recipe --recipe recipes/claude.yaml --bundle /path/to/bundle.md
cargo run -p yoetz -- browser recipe --recipe recipes/gemini.yaml --bundle /path/to/bundle.md
```
