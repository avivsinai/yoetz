use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn yoetz() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("yoetz").unwrap()
}

struct FrontierFixture {
    _dir: TempDir,
    config_path: PathBuf,
    registry_path: PathBuf,
}

fn frontier_fixture() -> FrontierFixture {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("yoetz.toml");
    let registry_path = dir.path().join("registry.json");

    fs::write(
        &config_path,
        r#"
[registry]
auto_sync_secs = 0
"#,
    )
    .unwrap();

    fs::write(&registry_path, frontier_registry_json()).unwrap();

    FrontierFixture {
        _dir: dir,
        config_path,
        registry_path,
    }
}

fn frontier_registry_json() -> String {
    serde_json::json!({
        "version": 1,
        "updated_at": "2026-03-21T00:00:00Z",
        "models": [
            {
                "id": "openai/gpt-5.4-pro",
                "context_length": 256000,
                "max_output_tokens": 16384,
                "pricing": {"completion_per_1k": 0.18},
                "provider": "openrouter"
            },
            {
                "id": "openai/gpt-5.4-mini",
                "context_length": 128000,
                "max_output_tokens": 8192,
                "pricing": {"completion_per_1k": 0.02},
                "provider": "openrouter"
            },
            {
                "id": "anthropic/claude-opus-4-6",
                "context_length": 1000000,
                "max_output_tokens": 32000,
                "pricing": {"completion_per_1k": 0.025},
                "provider": "openrouter"
            },
            {
                "id": "anthropic/claude-sonnet-4-6",
                "context_length": 200000,
                "max_output_tokens": 16384,
                "pricing": {"completion_per_1k": 0.012},
                "provider": "openrouter"
            },
            {
                "id": "google/gemini-3.1-pro-preview",
                "context_length": 1000000,
                "max_output_tokens": 8192,
                "pricing": {"completion_per_1k": 0.012},
                "provider": "openrouter"
            },
            {
                "id": "x-ai/grok-4.20-beta",
                "context_length": 200000,
                "max_output_tokens": 16384,
                "pricing": {"completion_per_1k": 0.006},
                "provider": "openrouter"
            },
            {
                "id": "meta-llama/llama-3.1-405b",
                "context_length": 32000,
                "max_output_tokens": 8192,
                "pricing": {"completion_per_1k": 0.004},
                "provider": "openrouter"
            },
            {
                "id": "deepseek/deepseek-v3.2-speciale",
                "context_length": 163840,
                "max_output_tokens": 8192,
                "pricing": {"completion_per_1k": 0.001},
                "provider": "openrouter"
            },
            {
                "id": "mistralai/mistral-large-2411",
                "context_length": 131072,
                "max_output_tokens": 8192,
                "pricing": {"completion_per_1k": 0.006},
                "provider": "openrouter"
            },
            {
                "id": "qwen/qwen-max",
                "context_length": 32768,
                "max_output_tokens": 8192,
                "pricing": {"completion_per_1k": 0.004},
                "provider": "openrouter"
            },
            {
                "id": "moonshotai/kimi-k2",
                "context_length": 128000,
                "max_output_tokens": 8192,
                "pricing": {"completion_per_1k": 0.003},
                "provider": "openrouter"
            },
            {
                "id": "reseller/one-off-model",
                "context_length": 64000,
                "max_output_tokens": 4096,
                "pricing": {"completion_per_1k": 0.123},
                "provider": "reseller"
            }
        ]
    })
    .to_string()
}

fn yoetz_with_frontier_fixture(fixture: &FrontierFixture) -> Command {
    let mut cmd = yoetz();
    cmd.env("YOETZ_CONFIG_PATH", &fixture.config_path)
        .env("YOETZ_REGISTRY_PATH", &fixture.registry_path)
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("XAI_API_KEY");
    cmd
}

#[test]
fn version_flag() {
    yoetz()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_flag() {
    yoetz()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("agent-friendly LLM council"));
}

#[test]
fn no_subcommand_shows_help() {
    yoetz()
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage"));
}

#[test]
fn ask_without_prompt_fails() {
    yoetz()
        .arg("ask")
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .assert()
        .failure();
}

#[test]
fn bundle_help() {
    yoetz()
        .args(["bundle", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--files"));
}

#[test]
fn council_help() {
    yoetz()
        .args(["council", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--models"));
}

#[test]
fn review_help() {
    yoetz().args(["review", "--help"]).assert().success();
}

#[test]
fn generate_help() {
    yoetz().args(["generate", "--help"]).assert().success();
}

#[test]
fn pricing_help() {
    yoetz().args(["pricing", "--help"]).assert().success();
}

#[test]
fn browser_recipe_help_mentions_var() {
    yoetz()
        .args(["browser", "recipe", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--var"));
}

#[test]
fn browser_attach_help_shows_cdp_flag() {
    yoetz()
        .args(["browser", "attach", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--cdp"));
}

#[test]
fn browser_check_help_shows_cdp_flag() {
    yoetz()
        .args(["browser", "check", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--cdp"));
}

#[test]
fn browser_recipe_help_shows_cdp_flag() {
    yoetz()
        .args(["browser", "recipe", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--cdp"));
}

#[test]
fn status_runs() {
    yoetz().arg("status").assert().success();
}

#[test]
fn invalid_subcommand() {
    yoetz()
        .arg("nonexistent")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));
}

#[test]
fn ask_em_dash_in_prompt_parses() {
    // Verifies that em-dashes in --prompt don't cause argument parsing errors.
    // The command will fail later (no API key / dry-run still needs provider),
    // but it must NOT fail with "unexpected argument '' found".
    let result = yoetz()
        .args([
            "ask",
            "--prompt",
            "summarize this \u{2014} and that",
            "--dry-run",
        ])
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("XAI_API_KEY")
        .assert();
    // Must not contain clap's argument parsing error
    result.stderr(predicate::str::contains("unexpected argument").not());
}

#[test]
fn bundle_em_dash_in_prompt_parses() {
    // Bundle with em-dash should parse without argument errors.
    // --all is required to avoid "files required" error.
    let result = yoetz()
        .args(["bundle", "--prompt", "review \u{2014} all code", "--all"])
        .assert();
    result.stderr(predicate::str::contains("unexpected argument").not());
}

#[test]
fn council_em_dash_in_prompt_parses() {
    let result = yoetz()
        .args([
            "council",
            "--prompt",
            "compare \u{2014} contrast",
            "--dry-run",
            "--models",
            "test/model",
        ])
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("XAI_API_KEY")
        .assert();
    result.stderr(predicate::str::contains("unexpected argument").not());
}

#[test]
fn models_frontier_help_shows_flags() {
    yoetz()
        .args(["models", "frontier", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--family"))
        .stdout(predicate::str::contains("--all"));
}

#[test]
fn models_frontier_default_output_is_curated() {
    let fixture = frontier_fixture();
    yoetz_with_frontier_fixture(&fixture)
        .args(["models", "frontier"])
        .assert()
        .success()
        .stdout(predicate::str::contains("FAMILY"))
        .stdout(predicate::str::contains("MODEL"))
        .stdout(predicate::str::contains("openai/gpt-5.4-pro"))
        .stdout(predicate::str::contains("reseller/one-off-model").not());
}

#[test]
fn models_frontier_family_filter_limits_output() {
    let fixture = frontier_fixture();
    yoetz_with_frontier_fixture(&fixture)
        .args(["models", "frontier", "--family", "openai"])
        .assert()
        .success()
        .stdout(predicate::str::contains("openai/gpt-5.4-pro"))
        .stdout(predicate::str::contains("anthropic/claude-opus-4-6").not())
        .stdout(predicate::str::contains("reseller/one-off-model").not());
}

#[test]
fn models_frontier_all_shows_every_family() {
    let fixture = frontier_fixture();
    yoetz_with_frontier_fixture(&fixture)
        .args(["models", "frontier", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("openai/gpt-5.4-pro"))
        .stdout(predicate::str::contains("anthropic/claude-opus-4-6"))
        .stdout(predicate::str::contains("reseller/one-off-model"));
}

#[test]
fn allow_unknown_flag_accepted() {
    yoetz()
        .args(["--allow-unknown", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--allow-unknown"));
}
