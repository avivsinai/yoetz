# yoetz

Fast, CLI-first LLM council + bundler + browser fallback for coding agents.

## Status

Phase 6 in progress: multimodal (vision + image/video generation).

## Quick start

```bash
# Bundle only
cargo run -p yoetz -- bundle --prompt "Review this" --files "src/**/*.rs" --format json

# Sync registry (OpenRouter + LiteLLM)
cargo run -p yoetz -- models sync --format json

# Ask (requires provider + model in config)
cargo run -p yoetz -- ask --prompt "Review this" --files "src/**/*.rs" --format json

# Ask with images (OpenAI Responses API)
cargo run -p yoetz -- ask --prompt "Describe this" --image /path/to/image.png --provider openai --model gpt-4.1

# Ask with video (Gemini API)
cargo run -p yoetz -- ask --prompt "Summarize this video" --video /path/to/clip.mp4 --provider gemini --model gemini-2.0-flash

# Council (multi-model)
cargo run -p yoetz -- council --prompt "Review this" --models openai/gpt-4o,anthropic/claude-3.5-sonnet

# Review current git diff
cargo run -p yoetz -- review diff --model openai/gpt-4o

# Review a single file
cargo run -p yoetz -- review file --path src/main.rs --model openai/gpt-4o

# Apply patch
yoetz apply --patch-file /tmp/patch.diff

# Generate images (OpenAI image_generation tool)
cargo run -p yoetz -- generate image --prompt "A cozy cabin in snow" --provider openai --model gpt-4.1 --n 2

# Generate video (OpenAI Sora)
cargo run -p yoetz -- generate video --prompt "Drone flyover of forest" --provider openai --model sora-2 --duration-secs 5

# Generate video (Gemini Veo)
cargo run -p yoetz -- generate video --prompt "A fox running through snow" --provider gemini --model veo-1 --duration-secs 5

# Browser passthrough (agent-browser must be installed)
cargo run -p yoetz -- browser exec -- open https://chatgpt.com/

# Run a recipe
cargo run -p yoetz -- browser recipe --recipe recipes/chatgpt.yaml --bundle /path/to/bundle.md
cargo run -p yoetz -- browser recipe --recipe recipes/claude.yaml --bundle /path/to/bundle.md
cargo run -p yoetz -- browser recipe --recipe recipes/gemini.yaml --bundle /path/to/bundle.md
```
