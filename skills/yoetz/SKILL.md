---
name: yoetz
version: 0.1.0
description: CLI-first LLM council + bundler. Use for fast second opinions, code review bundles, and multi-model consultations. Outputs JSON by default when YOETZ_AGENT=1.
---

# Yoetz Skill

## When to use
- Need a fast second opinion or review on local code
- Want a reproducible bundle (prompt + files + metadata)
- Need JSON output for automation
- Need browser fallback via agent-browser
- Need multi-model consensus (council)

## Core commands

### Ask (single model)
```bash
yoetz ask --prompt "Review this" --files "src/**/*.rs" --format json
```

### Council (multi-model)
```bash
yoetz council --prompt "Review" --models openai/gpt-5.2-codex,anthropic/claude-sonnet-4-5-20250929 --format json
```

### Review git diff
```bash
yoetz review diff --model openai/gpt-5.2-codex --format json
```

### Review single file
```bash
yoetz review file --path src/main.rs --model openai/gpt-5.2-codex --format json
```

### Bundle only (for manual paste or browser mode)
```bash
yoetz bundle --prompt "Explain this" --files "src/**/*.rs" --format json
```

### Pricing / budgets
```bash
yoetz pricing estimate --model openai/gpt-5.2 --input-tokens 12000 --output-tokens 800

yoetz ask --prompt "Review" --max-cost-usd 1.00 --daily-budget-usd 5.00
```

### Browser passthrough
```bash
yoetz browser exec -- snapshot --json
```

### Browser recipes
```bash
yoetz browser recipe --recipe recipes/chatgpt.yaml --bundle /path/to/bundle.md
yoetz browser recipe --recipe recipes/claude.yaml --bundle /path/to/bundle.md
yoetz browser recipe --recipe recipes/gemini.yaml --bundle /path/to/bundle.md
```

### Apply patch
```bash
yoetz apply --patch-file /tmp/patch.diff
```

## Agent contract
- Prefer `--format json` or `--format jsonl`.
- Keep outputs machine-readable; read artifacts from `~/.yoetz/sessions/<id>/`.
- If large bundles, run `yoetz bundle` first and inspect artifacts before `yoetz ask`.
- Gemini may return empty content if `--max-output-tokens` is too low; increase the limit if warned.
- Use `--debug` to capture raw Gemini responses during troubleshooting.
