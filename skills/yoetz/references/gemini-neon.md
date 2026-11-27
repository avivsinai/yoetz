# Gemini 4 Neon

Use a paid-tier Gemini API project with `GEMINI_API_KEY` set in the user's
environment. Yoetz sends ordinary text requests through Gemini's synchronous
`generateContent` route. The stable model ID is `gemini-4-neon`.
[Google's model card](https://aistudio.google.com/docs/models/gemini-4-neon).

## Resolve the model

Run these commands after setup or a registry refresh. The lookup must return
one exact direct Gemini entry.

```bash
yoetz models sync --format json
MODEL_ID=$(yoetz models list --provider gemini -s gemini-4-neon --format json \
  | jq -er '[.models[].id | select(. == "gemini/gemini-4-neon" or . == "gemini-4-neon")] | if length == 1 then .[0] else empty end')
test -n "$MODEL_ID" || { echo "Expected one direct Gemini 4 Neon registry entry" >&2; exit 1; }
```

If Neon is absent, save this catalog as `yoetz-models.json` in a user-managed
directory. If `org_registry_path` is already configured, add the entry to that
catalog's `models` list.

```json
{
  "version": 1,
  "models": [
    {
      "id": "gemini/gemini-4-neon",
      "provider": "gemini",
      "pricing": {},
      "capability": { "kind": "chat" }
    }
  ]
}
```

In trusted user configuration such as `~/.config/yoetz/config.toml`, add
`org_registry_path` under `[registry]` with the absolute path to that catalog.
Then rerun the lookup above.

```toml
[registry]
org_registry_path = "/absolute/path/to/yoetz-models.json"
```

## Ask for text

Use the returned `MODEL_ID` verbatim. This dry run checks selection without
submitting a request.

```bash
yoetz ask --provider gemini --model "$MODEL_ID" \
  -p "Explain the difference between latency and throughput." --dry-run --format json
```

Omit `--dry-run` when the user requests a billable API call. Use provider
defaults for thinking and output limits in this example. The catalog entry
supplies model identity without local pricing or token-limit estimates.
Yoetz reports the cost estimate as unknown for this entry. With
`--max-cost-usd` or `--daily-budget-usd`, Yoetz refuses the text request
before contacting Gemini because it needs an estimate to enforce the
budget.
Consult [Google's pricing](https://aistudio.google.com/docs/models/gemini-4-neon#pricing)
for billing. Report Gemini errors with the selected provider and model intact.
