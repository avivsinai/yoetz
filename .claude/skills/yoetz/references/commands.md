# Yoetz Command Reference

## Global Flags

| Flag | Description |
|------|-------------|
| `--format <json\|text>` | Output format (default: text) |
| `--debug` | Print raw provider responses |
| `--profile <name>` | Config profile to use |
| `--timeout-secs <N>` | Request timeout (default: 60) |
| `--output-final <path>` | Write final output to file |
| `--output-schema <path>` | JSON schema for structured output validation |

## Commands

### `ask` - Query a single LLM

```bash
yoetz ask -p "question" [options]
```

| Option | Description |
|--------|-------------|
| `-p, --prompt <text>` | Question text |
| `--prompt-file <path>` | Read prompt from file |
| `-f, --files <glob>` | File patterns to include as context |
| `--exclude <glob>` | File patterns to exclude |
| `--provider <name>` | LLM provider (openai, gemini, openrouter) |
| `--model <name>` | Model name |
| `--temperature <float>` | Sampling temperature (default: 0.1) |
| `--max-output-tokens <N>` | Max response tokens |
| `--image <path-or-url>` | Image input (repeatable) |
| `--image-mime <mime>` | Override image MIME type |
| `--video <path-or-url>` | Video input |
| `--video-mime <mime>` | Override video MIME type |
| `--response-format <json\|text>` | Request structured output |
| `--response-schema <path>` | JSON schema for response validation |
| `--dry-run` | Show what would be sent without calling API |
| `--max-cost-usd <float>` | Abort if estimated cost exceeds limit |
| `--daily-budget-usd <float>` | Daily spend cap |

### `council` - Multi-model consensus

```bash
yoetz council -p "question" --models <list> [options]
```

| Option | Description |
|--------|-------------|
| `--models <list>` | Comma-separated `provider/model` pairs (required) |

All `ask` options are also available.

### `review` - Code review

```bash
yoetz review diff [--staged] [options]
yoetz review file --path <file> [options]
```

### `bundle` - Package files for LLM context

```bash
yoetz bundle -p "context" -f <globs> [options]
```

| Option | Description |
|--------|-------------|
| `-f, --files <glob>` | File patterns to bundle |
| `--exclude <glob>` | Patterns to exclude |
| `--max-file-bytes <N>` | Max size per file (default: 200000) |
| `--max-total-bytes <N>` | Max total bundle size (default: 5000000) |

### `generate` - Create images and videos

```bash
yoetz generate image -p "description" --provider openai --model gpt-image-1
yoetz generate video -p "description" --provider openai --model sora-2-pro
yoetz generate video -p "description" --provider gemini --model veo-3.1-generate-preview
```

### `pricing` - Cost estimation

```bash
yoetz pricing estimate --model <model> --input-tokens <N> --output-tokens <N>
```

### `models` - List available models

```bash
yoetz models [--provider <name>]
```

### `browser` - Web UI fallback

```bash
yoetz browser login
yoetz browser check
yoetz browser sync-cookies
yoetz browser exec -- <command>
yoetz browser recipe --recipe <yaml> --bundle <path>
```

### `session` - Session management

```bash
yoetz session list
yoetz session show <id>
```

### `status` - Show configuration status

```bash
yoetz status
```

### `apply` - Apply code changes from review

```bash
yoetz apply <session-id>
```
